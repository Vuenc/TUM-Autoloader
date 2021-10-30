use std::io::BufWriter;
use futures::TryStreamExt;
use std::path::Path;
use futures::stream::{FuturesUnordered, FuturesOrdered};
use reqwest;
use regex::Regex;
use std::error::Error;
use std::fs::{self, create_dir};
use std::sync::Arc;
use std::{env::temp_dir, fs::File, io::Write};
use futures::{self, FutureExt};
use futures::TryFutureExt;
use futures::stream::StreamExt;
use select::document::Document;
use select::predicate::{Predicate, Attr, Class, Name, Text};
use simple_error::simple_error;
use reqwest_cookie_store::CookieStoreMutex;

#[tokio::main]
async fn main() -> GenericResult<()>{
    // let m3u8_url = "https://stream.lrz.de/vod/_definst_/mp4:tum/RBG/Sem_2021_10_21_12_15COMB.mp4/playlist.m3u8";
    // let download_future = download_video(m3u8_url, "out.mp4").await;
    // futures::executor::block_on(download_future).expect("Failed!");

    dotenv::dotenv().or(Err(simple_error!("'.env' file with credentials not found.")))?;
    let username = &std::env::var("TUM_USERNAME")?;
    let password = &std::env::var("TUM_PASSWORD")?;
    
    
    let moodle_auth_cookies = moodle_login(username, password).await?;
    
    // let course_url = "https://www.moodle.tum.de/course/view.php?id=57976"; // Intro 2 QC
    let course_url = "https://www.moodle.tum.de/course/view.php?idnumber=950576833"; // Programming Languages

    detect_moodle_videos(course_url, moodle_auth_cookies.clone()).await?;

    let tum_live_auth_cookies = tum_live_login(username, password).await?;

    let course_url = "https://live.rbg.tum.de/course/2021/W/Sem";

    detect_tum_live_videos(course_url, tum_live_auth_cookies).await?;

    Ok(())
}

#[derive(Debug)]
enum CourseVideo {
    TumLiveStream {
        url: String,
        lecture_title: String,
        video_title: String,
        date_time_string: String
    },
    MoodleVideoFile {
        url: String,
        lecture_title: String,
        section_title: String,
        video_title: String
    },
    PanoptoVideoFile {
        url: String,
        lecture_title: String,
        section_title: String,
        video_title: String
    }
}

/* TODOs
- parse live.rgb.tum lecture page, extract m3u8 urls
- (parse general live.rgb.tum page to display available lectures)
*/

type GenericError = Box<dyn Error + Send + Sync + 'static>;
type GenericResult<T> = Result<T, GenericError>;

async fn download_mp4(resp: reqwest::Response) -> GenericResult<()> {
    // let resp = resp?; Result<reqwest::Response, reqwest::Error>
    let filename = resp.url().to_string().split("/").last().ok_or(simple_error!("URL has no '/'"))?.to_owned();
    dbg!(&filename);
    if !filename.ends_with("mp4") {
        return Ok(())
    }
    let path = Path::new("download").join(filename);
    let mut writer = BufWriter::new(File::create(path)?);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        writer.write(&chunk?)?;
    }
    Ok(())
}

async fn detect_moodle_videos(course_url: &str, moodle_auth_cookies: Arc<CookieStoreMutex>) -> GenericResult<Vec<CourseVideo>> {
    let client = reqwest::Client::builder()
        .cookie_provider(moodle_auth_cookies.clone())
        .build()?;
        
    let resp = client.get(course_url).send().await?;
    let course_page_dom = Document::from(resp.text().await?.as_str());

    // Define before download_futures s.t. it is not dropped and referencing the Regex from a closure succeeds
    let panopto_video_url_regex = Regex::new(r#""VideoUrl":"(.*?)""#)?;
    let mut detect_futures: FuturesOrdered<futures::future::BoxFuture<_>> = FuturesOrdered::new();

    let lecture_title = course_page_dom.find(Class("page-header-headings")).next().unwrap().text();

    // Iterate through main sections to capture their titles
    for section_node in course_page_dom.find(Class("section").and(Class("main"))) {
        let section_title = section_node.find(Class("sectionname")).next().map_or(String::default(), |n| n.text());

        // Iterate through nodes that could be videos
        for video_node in section_node.find(
                Class("activityinstance") // Match videos that are directly linked as activity
                .or(Name("iframe").and(Attr("src", ())))) // Match embedded Panoptop players
            {
            let (lecture_title, section_title) = (lecture_title.clone(), section_title.clone());

            // Video that is directly linked as mp4
            if let Some("activityinstance") = video_node.attr("class") {
                let activity_url = video_node.find(Name("a")).next().unwrap().attr("href").unwrap();
                let video_title = video_node.find(Class("instancename")).next().unwrap().text(); //.map(|x| x.text())
                let detect_video_future = client.get(activity_url)
                    .send().err_into::<GenericError>()
                    .map_ok(|resp| {
                        let url = resp.url().to_string();
                        if url.ends_with("mp4") {
                            Some(CourseVideo::MoodleVideoFile {
                                url, lecture_title, section_title, video_title})
                        } else { None }});
                detect_futures.push(Box::pin(detect_video_future));
            }
            // Video in embedded Panopto player
            else if video_node.name() == Some("iframe") && video_node.attr("src").unwrap().contains("panopto") {
                // Recieve embedded player HTML and extract video url and title from it
                let panopto_url = video_node.attr("src").unwrap();
                let detect_video_future = client.get(panopto_url).send().err_into::<GenericError>()
                    .and_then(|resp| resp.text().err_into::<GenericError>())
                    .map_ok(|text| {
                        let panopto_dom = Document::from(text.as_str());

                        // The title is found in a heading with id 'title'
                        let video_title = panopto_dom.find(Attr("id", "title"))
                            .next().map_or(String::new(), |title| title.text().trim().to_owned());

                        // The url pointing to the video in mp4 format is found hardcoded in the embedded
                        // <script>, which is matched by `panopto_video_url_regex`
                        panopto_video_url_regex.captures(&text)
                            .and_then(|captures| captures.get(1))
                            .map(move |url_match| {
                                // In the JavaScript code, / is escaped as \/
                                let video_url = url_match.as_str().replace("\\/", "/");
                                CourseVideo::PanoptoVideoFile {url: video_url, lecture_title, section_title, video_title }
                            })
                    });
                detect_futures.push(Box::pin(detect_video_future));
            }
        }
    }

    let mut course_videos = vec![];
    while let Some(result) = detect_futures.next().await {
        match result {
            Ok(Some(course_video)) => {
                course_videos.push(course_video);
            },
            Ok(None) => {} // Ignore (e.g. pdf instead of mp4 files)
            error@Err(_) => { error?; } // Like this for now, but just skipping would also be an option
        }
    }
    
    Ok(course_videos)
}

fn extract_html_input_value<'a>(document: &'a Document, input_name: &str) -> GenericResult<&'a str> {
    document.find(Name("input")
        .and(Attr("name", input_name))).next()
        .and_then(|node| node.attr("value"))
        .ok_or(simple_error!("Could not find input with name '{}' and a 'value' attribute", input_name).into())
}

async fn moodle_login(username: &str, password: &str) -> GenericResult<Arc<CookieStoreMutex>> {
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
    let moodle_homepage_dom = Document::from(resp.text().await?.as_str());
    let login_url = moodle_homepage_dom.find(Name("a").descendant(Text))
        .find(|node| node.text().contains(LOGIN_LINK_TEXT))
        .and_then(|node| node.parent().unwrap().attr("href"))
        .ok_or(Box::new(simple_error!("Could not find link with text {} on moodle homepage", LOGIN_LINK_TEXT)))?;

    // Get csrf token from Shibboleth login form
    let resp = client.get(login_url).send().await?;
    let shibboleth_login_url = resp.url().to_string();
    let shibboleth_login_dom = Document::from(resp.text().await?.as_str());
    let csrf_token = extract_html_input_value(&shibboleth_login_dom, "csrf_token")?;

    // Post credentials, csrf token and additional params for Shibboleth login
    let params = [("j_username", username), ("j_password", password), 
        ("csrf_token", csrf_token), ("donotcache", "1"), ("_eventId_proceed", "")];
    let resp = client.post(shibboleth_login_url).form(&params).send().await?;

    // Parse Shibboleth response, extract SAMLResponse and RelayState
    let shibboleth_response_dom = Document::from(resp.text().await?.as_str());
    let saml_response = extract_html_input_value(&shibboleth_response_dom, "SAMLResponse")?;
    let relay_state = extract_html_input_value(&shibboleth_response_dom, "RelayState")?;

    // Post SAMLResponse and RelayState to finalize login
    let params = [("SAMLResponse", saml_response), ("RelayState", relay_state)];
    let post_url = shibboleth_response_dom.find(Name("form")).next()
        .and_then(|node| node.attr("action"))
        .ok_or(Box::new(simple_error!("Could not find form with 'action' attribute in Shibboleth response")))?;
    let resp = client.post(post_url).form(&params).send().await?;

    // Make sure the last request succeeded
    if resp.status().is_success() {
        Ok(cookie_store)
    } else {
        Err(simple_error!("Moodle Shibboleth login did not succeed!").into())
    }
}

async fn tum_live_login(username: &str, password: &str) -> GenericResult<Arc<CookieStoreMutex>> {
    // Login needs to store cookies, so our client needs a cookie store
    let cookie_store = Arc::new(reqwest_cookie_store::CookieStoreMutex::default());
    
    // Build a `reqwest` Client that uses the cookie store
    let client = reqwest::Client::builder()
        .cookie_provider(cookie_store.clone())
        .build()?;

    const TUM_LIVE_LOGIN_URL: &str = "https://live.rbg.tum.de/login";
    let params = [("username", username), ("password", password)];
    let resp = client.post(TUM_LIVE_LOGIN_URL).form(&params).send().await?;

    if resp.status().is_success() {
        Ok(cookie_store)
    } else {
        Err(simple_error!("TUM Live login not successful.").into())
    }
}

async fn detect_tum_live_videos(course_url: &str, tum_live_auth_cookies: Arc<CookieStoreMutex>) -> GenericResult<Vec<CourseVideo>> {
    let client = reqwest::Client::builder()
        .cookie_provider(tum_live_auth_cookies.clone())
        .build()?;
    
    let resp = client.get(course_url).send().await?;
    let course_page_dom = Document::from(resp.text().await?.as_str());

    let lecture_title = course_page_dom.find(Class("text-1")).next().map(|node| node.text().trim().to_owned()).unwrap_or_default();

    let recording_nodes = course_page_dom.find(Name("a").and(Class("text-3")).and(Attr("href", ())));
    let course_videos = recording_nodes.map(|node| {
        let video_url = node.attr("href").unwrap().to_owned();
        let video_title = node.text().trim().to_owned();
        let date_time_string = node.parent()
            .and_then(|parent| parent.parent())
            .and_then(|parent| parent.find(Class("text-5").child(Text)).last())
            .map(|date_time_node| date_time_node.text().trim().to_owned())
            .unwrap_or_default();
        CourseVideo::TumLiveStream {
            url: video_url, video_title, date_time_string, lecture_title: lecture_title.clone()
        }
    }).collect();

    Ok(course_videos)
}

async fn process_lecture(lecture_page_url: &str) -> Result<(), reqwest::Error> {
    let body = reqwest::get(lecture_page_url).await?
        .text().await?;

    let video_page_url = "";
    let body = reqwest::get(lecture_page_url).await?
    .text().await?;
    Ok(())
}

fn is_no_comment(line: &&str) -> bool {
    line.chars().next() != Some('#')
}

async fn download_video(m3u8_url: &str, out_path: &str) -> Result<(), reqwest::Error> {
    let chunklist_id = reqwest::get(m3u8_url).await?.text().await?
        .lines().filter(is_no_comment).next().unwrap().to_owned();

    let mp4_base_url = m3u8_url.rmatch_indices("/").next()
        .map(|(last_slash_index, _)| &m3u8_url[..last_slash_index])
        .unwrap();

    let m3u8_content = reqwest::get([mp4_base_url, &chunklist_id].join("/")).await?.text().await?;
    let m3u8_ts_lines = m3u8_content.lines()
        .filter(is_no_comment)
        .collect::<Vec<_>>();

    let ts_dir = temp_dir().join(chunklist_id);
    fs::create_dir(&ts_dir).unwrap();

    let mut download_futures = FuturesUnordered::new();

    for (i, line) in m3u8_ts_lines.iter().enumerate() {
        let ts_url = [mp4_base_url, line].join("/");

        let path = ts_dir.join(i.to_string());
        let download_future = reqwest::get(ts_url)
            .and_then(|response| response.bytes())
            .and_then(move |ts_contents| {
                std::fs::write(path, ts_contents)
                    .expect("Couldn't write to file...");
                futures::future::ok::<(), reqwest::Error>(())
            })
            .or_else(|e| {
                println!("Error: {:?}", e);
                futures::future::ok::<(), reqwest::Error>(())
            });
        download_futures.push(download_future);

        if download_futures.len() >= 150 {
            download_futures.next().await;
        }
        // let ts_contents = reqwest::get(ts_url).await?.bytes().await?;
    }
    // download_futures.;
    while let Some(_) = download_futures.next().await {}
    dbg!("Wrote to file!");

    let mut mp4_out_file = std::fs::File::create(out_path)
        .expect("Couldn't create out file.");

    for i in 0..m3u8_ts_lines.len() {
        let bytes = std::fs::read(ts_dir.join(i.to_string()))
            .expect("Couldn't read ts file...");
        mp4_out_file.write(&bytes)
            .expect("Couldn't write bytes to file...");
    }

    dbg!("CATed!");

    Ok(())
}
