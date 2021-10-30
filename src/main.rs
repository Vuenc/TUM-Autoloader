use std::io::BufWriter;
use std::path::Path;
use futures::stream::FuturesUnordered;
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
use envfile::EnvFile;

#[tokio::main]
async fn main() -> GenericResult<()>{
    // let m3u8_url = "https://stream.lrz.de/vod/_definst_/mp4:tum/RBG/Sem_2021_10_21_12_15COMB.mp4/playlist.m3u8";
    // let download_future = download_video(m3u8_url, "out.mp4").await;
    // futures::executor::block_on(download_future).expect("Failed!");

    let credentials = EnvFile::new("credentials.env")?;
    // let credentials = [("username", credentials_envfile.get("username")), 
    //     ("password", credentials_envfile.get("password"))];

    let moodle_auth_cookies = moodle_login(
        credentials.get("username").ok_or(simple_error!("'username' missing from credentials.env file"))?,
        credentials.get("password").ok_or(simple_error!("'password' missing from credentials.env file"))?)
        .await.unwrap();
    moodle_download_mp4s(moodle_auth_cookies).await.unwrap();

    Ok(())
}


/* TODOs
- parse live.rgb.tum lecture page, extract m3u8 urls
- (parse general live.rgb.tum page to display available lectures)
*/

type GenericError = Box<dyn Error>;
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
        writer.write(&chunk?).unwrap();
    }
    Ok(())
}

async fn moodle_download_mp4s(moodle_auth_cookies: Arc<reqwest_cookie_store::CookieStoreMutex>) -> GenericResult<()> {
    let client = reqwest::Client::builder()
        .cookie_provider(moodle_auth_cookies.clone())
        .build().unwrap();
    // let course_url = "https://www.moodle.tum.de/course/view.php?id=68910";
    let course_url = "https://www.moodle.tum.de/course/view.php?id=57976";
    let resp = client.get(course_url).send().await?;
    let course_page_dom = Document::from(resp.text().await?.as_str());

    let panopto_video_url_regex; // Define before download_futures s.t. it is not dropped and referencing the Regex from a closure succeeds

    let mut download_futures: FuturesUnordered<futures::future::BoxFuture<_>> = FuturesUnordered::new();

    if !Path::new("./download").exists() {
        create_dir("./download")?;
    }

    for activity_instance in course_page_dom.find(Class("activityinstance")) {
        // dbg!(activity_instance.find(Class("instancename")).next().map(|x| x.text()));
        let activity_url = activity_instance.find(Name("a")).next().unwrap().attr("href").unwrap();
        let resp_future = client.get(activity_url)
            .send().err_into::<GenericError>()
            .and_then(download_mp4);
        download_futures.push(Box::pin(resp_future));        
        if download_futures.len() >= 5 {
            download_futures.next().await;
        }
    }

    panopto_video_url_regex = Regex::new(r#""VideoUrl":"(.*?)""#).unwrap();

    for panopto_iframe in course_page_dom.find(Name("iframe").and(Attr("src", ()))) {
        let panopto_url = panopto_iframe.attr("src").unwrap();
        let resp_future = client.get(panopto_url).send()
            .then(|resp| async {
                let panopto_frame_html = resp.unwrap().text().await.unwrap();
                let video_url = panopto_video_url_regex.captures(&panopto_frame_html)
                    .unwrap()[1].replace("\\/", "/");
                // dbg!(video_url);
                client.get(video_url).send().err_into::<GenericError>().await
            })
            .and_then(download_mp4);
        download_futures.push(Box::pin(resp_future));        
        if download_futures.len() >= 5 {
            download_futures.next().await;
        }
    }
    while let Some(_) = download_futures.next().await {}

    Ok(())
}

fn extract_html_input_value<'a>(document: &'a Document, input_name: &str) -> GenericResult<&'a str> {
    document.find(Name("input")
        .and(Attr("name", input_name))).next()
        .and_then(|node| node.attr("value"))
        .ok_or(simple_error!("Could not find input with name '{}' and a 'value' attribute", input_name).into())
}

async fn moodle_login(username: &str, password: &str) -> GenericResult<Arc<reqwest_cookie_store::CookieStoreMutex>> {
    // Shibboleth login need to store cookies, so our client needs a cookie store
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
