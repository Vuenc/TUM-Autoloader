use reqwest::{self, Url};
use std::{collections::HashSet, fmt::Display, pin::Pin, sync::Arc};
use regex::Regex;
use futures::{self, TryFutureExt, stream::{StreamExt, FuturesOrdered}};
use select::{document::Document,
            predicate::{Predicate, Attr, Class, Name, Text}};
use simple_error::simple_error;
use reqwest_cookie_store::CookieStoreMutex;

use crate::{GenericError, GenericResult, data::{CourseFileMetadata, CourseFileResource, CourseFile}};

#[derive(Debug)]
pub struct MoodleCrawlingError {
    pub successful_detections: Vec<CourseFile>,
    pub failed_detections: Vec<GenericError>
}

impl Display for MoodleCrawlingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for MoodleCrawlingError {}

pub async fn detect_moodle_files(course_url: &str, moodle_auth_cookies: Arc<CookieStoreMutex>) 
        -> GenericResult<Vec<CourseFile>>
{
    let client = reqwest::Client::builder()
        .cookie_provider(moodle_auth_cookies.clone())
        .build()?;
        
    // Define before `download_futures` s.t. it is not dropped too early and referencing the Regex from a closure succeeds
    let panopto_video_url_regex = Regex::new(r#""VideoUrl":"(.*?)""#)?;
    let mut detect_futures: FuturesOrdered<futures::future::BoxFuture<_>> = FuturesOrdered::new();
    let mut crawled_urls = HashSet::new();
    
    let resp = client.get(course_url).send().await?;
    { // Artificial scope s.t. the non-`Send` `course_page_dom` is dropped before the next .await
        let course_page_dom = Document::from(resp.text().await?.as_str());
        let lecture_title = course_page_dom.find(Class("page-header-headings")).next().map(|n| n.text()).unwrap_or_default();

        // Iterate through main sections to capture their titles
        for section_node in course_page_dom.find(Class("section").and(Class("main"))) {
            let section_title = section_node.find(Class("sectionname")).next().map(|n| n.text()).unwrap_or_default();
           
            // Iterate through nodes that could be directly linked videos/documents
            for activity_node in section_node.find(Class("activityinstance"))
            {
                if let Some(activity_url) = activity_node.find(Name("a")).next()
                    .and_then(|n| n.attr("href"))
                {
                    if crawled_urls.insert(activity_url) {
                        let activity_title = activity_node.find(Class("instancename")).next().map(|n| n.text()).unwrap_or_default();
                        // TODO: Handle nested activity urls recursively (not only here!)
                        let detect_future = detect_moodle_course_file(&client, activity_url, lecture_title.clone(), section_title.clone(), activity_title);
                        detect_futures.push(detect_future);
                    }
                }
            }

            // Iterate through nodes that could be embedded Panopto players
            for video_node in section_node.find(Name("iframe").and(Attr("src", ())))
            {
                // Video in embedded Panopto player
                if video_node.name() == Some("iframe") && video_node.attr("src").unwrap().contains("panopto") {
                    // Recieve embedded player HTML and extract video url and title from it
                    let panopto_url = video_node.attr("src").unwrap();
                    let (lecture_title, section_title) = (lecture_title.clone(), section_title.clone());
                    let detect_video_future = client.get(panopto_url).send().err_into::<GenericError>()
                        .and_then(|resp| resp.text().err_into::<GenericError>())
                        .map_ok(|text| {
                            // The title is found in a heading with id 'title'
                            let video_title = {
                                let panopto_dom = Document::from(text.as_str());
                                panopto_dom.find(Attr("id", "title"))
                                    .next().map_or(String::new(), |title| title.text().trim().to_owned())
                            };

                            // The url pointing to the video in mp4 format is found hardcoded in the embedded
                            // <script>, which is matched by `panopto_video_url_regex`
                            panopto_video_url_regex.captures(&text)
                                .and_then(|captures| captures.get(1))
                                .and_then(move |url_match| {
                                    // In the JavaScript code, / is escaped as \/
                                    let video_url = url_match.as_str().replace("\\/", "/");
                                    moodle_course_file(video_url, lecture_title, section_title, video_title)
                                })
                        });
                    detect_futures.push(Box::pin(detect_video_future));
                }
            }
        }

        // As fallback capture every link inside a "role=main" element (that has not been captured before)
        for main_content_element in course_page_dom.find(Attr("role", "main"))
        {
            for link_node in main_content_element.find(Name("a")) {
                if let Some(link_url) = link_node.attr("href") {
                    // Insert the link url and only continue if not yet present
                    if crawled_urls.insert(link_url) {
                        let detect_future = detect_moodle_course_file(&client, link_url, lecture_title.clone(), String::new(), link_node.text());
                        detect_futures.push(detect_future);
                    }
                }
            }
        }
    }

    let mut course_files = vec![];
    let mut errors: Vec<GenericError> = vec![];
    while let Some(result) = detect_futures.next().await {
        match result {
            Ok(Some(course_video)) => {
                course_files.push(course_video);
            },
            Ok(None) => {} // Ignore (e.g. unwanted file types)
            Err(error) => { errors.push(error.into()); } // Like this for now, but just skipping would also be an option
        }
    }
    if errors.len() == 0 {
        Ok(course_files)
    } else {
        Err(MoodleCrawlingError{successful_detections: course_files, failed_detections: errors}.into())
    }
}

// Follows an URL through redirects and builds a CourseFile from the final url.
fn detect_moodle_course_file(client: &reqwest::Client, activity_url: &str, lecture_title: String, section_title: String, activity_title: String)
        -> Pin<Box<impl futures::Future<Output=GenericResult<Option<CourseFile>>>>>
{
    let detect_file_future = client.get(activity_url)
        .send().err_into::<GenericError>()
        // store the final url (after redirects)
        .map_ok(|resp| {
            let url = resp.url().to_string();
            moodle_course_file(url, lecture_title, section_title, activity_title)
        });
    return Box::pin(detect_file_future);
}

fn moodle_course_file(url: String, lecture_title: String, section_title: String, activity_title: String) -> Option<CourseFile> {
    let metadata = CourseFileMetadata::MoodleActivity { lecture_title, section_title, activity_title };
    let url_parsed = Url::parse(&url);
    let file_extension = url_parsed.ok()
        .and_then(|url_parsed| url_parsed.path_segments()
        .and_then(|p| p.into_iter().last())
        .map(|s| s.rfind(".")
        .map(|i| (&s[i+1..]).to_lowercase())));

    let resource = match file_extension {
        Some(Some(extension)) => {
            match extension.as_str() {
                "mp4" => Some(CourseFileResource::Mp4File {url}),
                "m3u8" => Some(CourseFileResource::HlsStream {main_m3u8_url: url}),
                "html" | "php" => None, // HTML and PHP files are ignored
                _ => Some(CourseFileResource::Document {url, file_extension: Some(extension)})
            }
        }
        // Some(None) means: URL parsing worked, but there is not . indicating a file extension. For now: ignore such files.
        Some(None) => None, // alternatively: Some(CourseFileResource::Document {url, file_extension: None}),
        // None means: URL parsing failed
        None => None
    };
    resource.map(|resource| CourseFile { metadata, resource })
}

fn extract_html_input_value<'a>(document: &'a Document, input_name: &str) -> GenericResult<&'a str> {
    document.find(Name("input")
        .and(Attr("name", input_name))).next()
        .and_then(|node| node.attr("value"))
        .ok_or(simple_error!("Could not find input with name '{}' and a 'value' attribute", input_name).into())
}

pub async fn moodle_login(username: &str, password: &str) -> GenericResult<Arc<CookieStoreMutex>> {
    // Shibboleth login needs to store cookies, so our client needs a cookie store
    let cookie_store = Arc::new(reqwest_cookie_store::CookieStoreMutex::default());
    
    // Build a `reqwest` Client that uses the cookie store
    let client = reqwest::Client::builder()
        .cookie_provider(cookie_store.clone())
        .build()?;

    const MOODLE_URL: &str = "https://www.moodle.tum.de/";
    const LOGIN_LINK_TEXT: &str = "TUM-Kennung";

    // Get Shibboleth login url by parsing moodle homepage
    let resp = client.get(MOODLE_URL).send().await?;
    let login_url = {
        let moodle_homepage_dom = Document::from(resp.text().await?.as_str());
        moodle_homepage_dom.find(Name("a").descendant(Text))
            .find(|node| node.text().contains(LOGIN_LINK_TEXT))
            .and_then(|node| node.parent().unwrap().attr("href"))
            .ok_or(Box::new(simple_error!("Could not find link with text {} on moodle homepage", LOGIN_LINK_TEXT)))?.to_owned()
    };

    // Get csrf token from Shibboleth login form
    let resp = client.get(login_url).send().await?;
    let shibboleth_login_url = resp.url().to_string();
    let csrf_token = {
        let shibboleth_login_dom = Document::from(resp.text().await?.as_str());
        extract_html_input_value(&shibboleth_login_dom, "csrf_token")?.to_owned()
    };

    // Post credentials, csrf token and additional params for Shibboleth login
    let params = [("j_username", username), ("j_password", password), 
        ("csrf_token", &csrf_token), ("donotcache", "1"), ("_eventId_proceed", "")];
    let resp = client.post(shibboleth_login_url).form(&params).send().await?;

    // Parse Shibboleth response, extract SAMLResponse and RelayState
    let (saml_response, relay_state, post_url) = {
        let shibboleth_response_dom = Document::from(resp.text().await?.as_str());
        let saml_response = extract_html_input_value(&shibboleth_response_dom, "SAMLResponse")?.to_owned();
        let relay_state = extract_html_input_value(&shibboleth_response_dom, "RelayState")?.to_owned();
        let post_url = shibboleth_response_dom.find(Name("form")).next()
            .and_then(|node| node.attr("action"))
            .ok_or(Box::new(simple_error!("Could not find form with 'action' attribute in Shibboleth response")))?.to_owned();
        (saml_response, relay_state, post_url)
    };

    // Post SAMLResponse and RelayState to finalize login
    let params = [("SAMLResponse", saml_response), ("RelayState", relay_state)];
    let resp = client.post(post_url).form(&params).send().await?;

    // Make sure the last request succeeded
    if resp.status().is_success() {
        Ok(cookie_store)
    } else {
        Err(simple_error!("Moodle Shibboleth login did not succeed!").into())
    }
}
