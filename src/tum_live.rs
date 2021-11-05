use reqwest;
use std::sync::Arc;
use select::{document::Document, predicate::{Predicate, Attr, Class, Name, Text}};
use simple_error::simple_error;
use reqwest_cookie_store::CookieStoreMutex;

use crate::GenericResult;
use crate::data::CourseVideo;

pub async fn tum_live_login(username: &str, password: &str) -> GenericResult<Arc<CookieStoreMutex>> {
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

pub async fn detect_tum_live_videos(course_url: &str, tum_live_auth_cookies: Arc<CookieStoreMutex>) -> GenericResult<Vec<CourseVideo>> {
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
