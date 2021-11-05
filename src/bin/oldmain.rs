use futures::TryFutureExt;
use tum_autoloader::{GenericError, GenericResult, 
    download::{download_mp4},
    moodle::{moodle_login, detect_moodle_videos},
    tum_live::{tum_live_login, detect_tum_live_videos}};
use simple_error::simple_error;


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

    let moodle_videos = detect_moodle_videos(course_url, moodle_auth_cookies.clone()).await?;
    for course_video in moodle_videos.iter().skip(moodle_videos.len() - 6) {
        let resp = reqwest::get(course_video.url())
            .err_into::<GenericError>()
            .and_then(download_mp4).await?;
    }

    let tum_live_auth_cookies = tum_live_login(username, password).await?;

    let course_url = "https://live.rbg.tum.de/course/2021/W/Sem";

    detect_tum_live_videos(course_url, tum_live_auth_cookies).await?;

    Ok(())
}
