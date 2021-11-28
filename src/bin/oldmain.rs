use std::{path::{Path, PathBuf}, pin::Pin};

use futures::{Future, StreamExt, TryFutureExt, stream::{FuturesOrdered}};
use tum_autoloader::{GenericError, GenericResult, 
    download::{download_mp4},
    moodle::{moodle_login, detect_moodle_videos},
    tum_live::{tum_live_login, detect_tum_live_videos}};
use simple_error::simple_error;
use tum_autoloader::data::{Course, CourseFileDownload, CourseVideo, CourseType, DownloadState, AutoDownloadMode, PostprocessingStep};
use serde_json;
use tum_autoloader::postprocessing::perform_postprocessing_step;

const STATE_FILE_PATH: &str = "../../Studium/TUM Recordings/autoloader.json";
const RECHECK_INTERVAL_SECONDS: u64 = 60 * 1; // 30 minutes

#[tokio::main]
async fn main() -> GenericResult<()>{
    // let m3u8_url = "https://stream.lrz.de/vod/_definst_/mp4:tum/RBG/Sem_2021_10_21_12_15COMB.mp4/playlist.m3u8";
    // let download_future = download_video(m3u8_url, "out.mp4").await;
    // futures::executor::block_on(download_future).expect("Failed!");

    dotenv::dotenv().or(Err(simple_error!("'.env' file with credentials not found.")))?;
    let username = &std::env::var("TUM_USERNAME")?;
    let password = &std::env::var("TUM_PASSWORD")?;

    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(RECHECK_INTERVAL_SECONDS));

    let mut courses = match load_courses(STATE_FILE_PATH) {
        Ok(courses) => courses,
        Err(_) => {
            vec![ Course {
                url: "https://www.moodle.tum.de/course/view.php?idnumber=950576833".into(),
                name: "Programming Languages".into(),
                course_type: CourseType::Moodle,
                video_download_directory: PathBuf::from("../../Studium/TUM Recordings/"),
                file_download_directory: PathBuf::from("../../Studium/Programming Languages/"),
                auto_download_mode: AutoDownloadMode::Videos,
                videos: vec![],
                max_keep_days_videos: None,
                max_keep_videos: None,
                video_post_processing_steps: vec![PostprocessingStep::FfmpegReencode {target_fps: 30}]
            }]
        }
    };

    loop {
        interval.tick().await;
        let new_videos_count = check_for_updates(&mut courses, &username, &password).await?;

        println!("{} new videos discovered.", new_videos_count);

        // let k = courses[0].videos.len() - 0;
        // for video in courses[0].videos[0..k].iter_mut() {
        //     video.download_state = DownloadState::None
        // }

        if new_videos_count > 0 {
            let downloads_result = process_downloads(&mut courses, 1).await;
            let successful_downloads_indices =
            match &downloads_result {
                Ok(successful_downloads_indices) | Err((successful_downloads_indices, _)) => {
                    println!("Downloaded {} new videos.", successful_downloads_indices.len());
                    successful_downloads_indices
                }
            };
            if let Err((_, failed_downloads)) = &downloads_result {
                println!("Failed to download {} videos.", failed_downloads.len());
                for (course_index, video_index, error) in failed_downloads {
                    let course = &courses[*course_index];
                    let video = &course.videos[*video_index].file;
                    println!("Download of {} - {} failed:", course.name, video.video_title());
                    println!("{}", error);
                }
            }

            perform_postprocessing(&courses, &successful_downloads_indices)?;
        }
        save_courses(STATE_FILE_PATH, &courses)?;
    }
}

async fn check_for_updates(courses: &mut Vec<Course>, tum_username: &str, tum_password: &str) -> GenericResult<i32> {
    let mut new_videos_count = 0;
    let moodle_auth_cookies = moodle_login(tum_username, tum_password).await?;

    for course in courses {
        match course.course_type {
            CourseType::Moodle => {
                let mut moodle_videos = detect_moodle_videos(&course.url, moodle_auth_cookies.clone()).await?;

                // Deduplicate found videos and update availability information
                for existing_course_video in &mut course.videos {
                    if let Some((i, _)) = moodle_videos.iter().enumerate().find(
                            |(_, v)| v.url() == existing_course_video.file.url()) {
                        moodle_videos.remove(i);
                    } else {
                        existing_course_video.available = false;
                    }
                }
                
                // Add newly found videos to `course.videos`
                for course_video in moodle_videos {
                    let video_download_data = CourseFileDownload {
                        file: course_video,
                        available: true,
                        download_state: if course.auto_download_videos_enabled() {DownloadState::Requested} else {DownloadState::None},
                        discovery_time: chrono::Utc::now(),
                        download_time: None
                    };
                    assert!(video_download_data.file.url().ends_with("mp4"));
                    course.videos.push(video_download_data);
                    new_videos_count += 1;
                }
                
            },
            CourseType::TumLive => todo!(),
            CourseType::GenericWebsite => todo!(),
        }
    }
    Ok(new_videos_count)
}

async fn process_downloads(courses: &mut Vec<Course>, max_parallel_downloads: usize)
        -> Result<Vec<(usize, usize)>, (Vec<(usize, usize)>, Vec<(usize, usize, GenericError)>)> {
    let mut download_futures = FuturesOrdered::new();

    // Store course and video indices of all, successful and failed downloads
    // s.t. after the loop, when the mutable borrow of courses has ended, the download states can be updated.
    let mut attempted_downloads_indices = vec![];
    let mut download_results = vec![];

    // Iterating over all videos of all courses
    for (i, course) in courses.iter_mut().enumerate() {
        for (j, video) in course.videos.iter_mut().enumerate() {

            // If the video is requested to be downloaded:
            if video.download_state == DownloadState::Requested {
                // Mark download as attempted, construct a download future depending on the video type.
                attempted_downloads_indices.push((i, j));
                let download_future: Pin<Box<dyn Future<Output = GenericResult<()>>>> =
                match &video.file {
                    CourseVideo::MoodleVideoFile { url, .. } => {
                        // For moodle videos: identify target filename from url
                        match url.split("/").last() {
                            Some(filename) => {
                                let path = course.video_download_directory.join(filename.to_owned());
                                // Set download state to running and build the download future
                                video.download_state = DownloadState::Running(path.clone());
                                Box::pin(reqwest::get(url)
                                    .err_into::<GenericError>()
                                    .and_then(move |response| download_mp4(response, path)))
                            }
                            None => {
                                // If no filename can be identified: add future indicating this failure
                                Box::pin(async { Err(simple_error!("URL has no '/'").into()) })
                                // unsuccessful_downloads_indices.push((i, j, simple_error!("URL has no '/'").into()));
                                // continue 'video_loop
                            }
                        }
                    },
                    CourseVideo::TumLiveStream { url, lecture_title, video_title, date_time_string } => todo!(),
                    CourseVideo::PanoptoVideoFile { url, lecture_title, section_title, video_title } => todo!(),
                };
                // Push download future into running queue
                download_futures.push(download_future);
            }

            // Ensure that at most `max_parallel_downloads` download run in parallel, store the result
            if download_futures.len() >= max_parallel_downloads {
                let result = download_futures.next().await.unwrap();
                download_results.push(result);
            }
        }
    }

    // Make sure that all downloads are finished
    while download_futures.len() >= max_parallel_downloads {
        let result = download_futures.next().await.unwrap();
        download_results.push(result);
    }
    drop(download_futures);

    let mut successful_downloads_indices = vec![];
    let mut unsuccessful_downloads_indices = vec![];
    // For each result and corresponding course/video index:
    for (&(course_index, video_index), result) in attempted_downloads_indices.iter().zip(download_results) {
        match result {
            Ok(_) => {
                // If the download was successful: set state to `Completed`
                if let DownloadState::Running(ref path) = courses[course_index].videos[video_index].download_state {
                    courses[course_index].videos[video_index].download_state = DownloadState::Completed(path.clone());
                    courses[course_index].videos[video_index].download_time = Some(chrono::Utc::now());
                    successful_downloads_indices.push((course_index, video_index))
                }
            },
            Err(error) => {
                courses[course_index].videos[video_index].download_state = DownloadState::Failed;
                unsuccessful_downloads_indices.push((course_index, video_index, error))
            }
        }
    }

    if unsuccessful_downloads_indices.len() == 0 {
        return Ok(successful_downloads_indices)
    } else {
        // Return an error with the list of failed downloads if there are any
        return Err((successful_downloads_indices, unsuccessful_downloads_indices))
    }
}


fn save_courses<P>(path: P, courses: &Vec<Course>) -> GenericResult<()>
        where P: AsRef<Path> {
    let json_courses = serde_json::to_string_pretty(&courses)?;
    std::fs::write(path, json_courses)?;
    Ok(())
}

fn load_courses<P>(path: P) -> GenericResult<Vec<Course>>
        where P: AsRef<Path> {
    let json_courses = String::from_utf8(std::fs::read(path)?)?;
    let courses = serde_json::from_str(&json_courses)?;
    Ok(courses)
}

fn perform_postprocessing(courses: &Vec<Course>, postprocessing_course_videos_ids: &[(usize, usize)]) -> GenericResult<()> {
    for &(course_id, video_id) in postprocessing_course_videos_ids {
        let course = &courses[course_id];
        let video = &course.videos[video_id];
        for step in &course.video_post_processing_steps {
            perform_postprocessing_step(video, step)?;
        }
    }
    Ok(())
}
