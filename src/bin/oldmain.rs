use std::{fmt::Display, path::{Path, PathBuf}, pin::Pin};

use futures::{Future, StreamExt, TryFutureExt, stream::{FuturesOrdered}};
use tum_autoloader::{GenericError, GenericResult, data::CourseFileResource, download::{download_mp4, download_document},
    moodle::{MoodleCrawlingError, detect_moodle_files, moodle_login},
    tum_live::{tum_live_login, detect_tum_live_videos}};
use simple_error::simple_error;
use tum_autoloader::data::{Course, CourseFileDownload, CourseType, DownloadState, AutoDownloadMode, PostprocessingStep};
use serde_json;
use tum_autoloader::postprocessing::perform_postprocessing_step;
use structopt::StructOpt;

// const STATE_FILE_PATH: &str = "../../Studium/TUM Recordings/autoloader.json";
// const RECHECK_INTERVAL_SECONDS: u64 = 60 * 1; // 30 minutes

#[derive(StructOpt)]
#[structopt(name = "tum-autoloader", about = "Automatically download lecture recordings and files from TUM websites.")]
struct CommandLineOptions {
    /// Repeatedly check every `repeat_interval` minutes. If not set, run once and exit.
    #[structopt(long)]
    repeat_interval: Option<u64>,

    /// JSON file where the program stores its state. Default: "autoloader.json".
    #[structopt(long, parse(from_os_str), default_value="autoloader.json")]
    state_file: PathBuf,

    /// .env file where `TUM_USERNAME` and `TUM_PASSWORD` are stored. Default: ".env".
    #[structopt(long, parse(from_os_str), default_value=".env")]
    credentials_file: PathBuf,

    /// In `discover` mode, videos/documents are only discovered, but set to not be automatically downloaded.
    #[structopt(long)]
    discover: bool
}

#[tokio::main]
async fn main() -> GenericResult<()>{
    let commandline_options = CommandLineOptions::from_args();

    dotenv::from_path(commandline_options.credentials_file)
        .or(Err(simple_error!("'.env' file with credentials not found.")))?;
    let username = &std::env::var("TUM_USERNAME")?;
    let password = &std::env::var("TUM_PASSWORD")?;

    let mut interval = commandline_options.repeat_interval.map(|interval_minutes|
        tokio::time::interval(tokio::time::Duration::from_secs(interval_minutes * 60)));

    let mut courses = match load_courses(&commandline_options.state_file) {
        Ok(courses) => courses,
        Err(err) => {
            println!("Error loading state: {:}", err);
            println!("Using default configuration...");
            vec![ Course {
                url: "https://www.moodle.tum.de/course/view.php?idnumber=950576833".into(),
                name: "Programming Languages".into(),
                course_type: CourseType::Moodle,
                video_download_directory: PathBuf::from("../../Studium/TUM Recordings/"),
                file_download_directory: PathBuf::from("../../Studium/Programming Languages/"),
                auto_download_mode: AutoDownloadMode::Videos,
                files: vec![],
                max_keep_days_videos: None,
                max_keep_videos: None,
                video_post_processing_steps: vec![PostprocessingStep::FfmpegReencode {target_fps: 30}]
            }]
        }
    };

    let mut continue_next_check = true;
    while continue_next_check {
        if let Some(interval) = &mut interval {
            interval.tick().await;
        }
        let check_for_updates_result = check_for_updates(&mut courses, &username, &password).await;
        let new_videos_count = match check_for_updates_result {
            Ok(count) => count,
            Err(error) => {
                if error.downcast_ref::<CheckForUpdatesError>().is_some() {
                    if let Ok(check_for_updates_error) = error.downcast::<CheckForUpdatesError>() {
                        println!("Errors occured while checking for updates.");
                        for error in check_for_updates_error.errors {
                            println!("{}", error);
                        }
                        check_for_updates_error.new_videos_count
                    } else { unreachable!() }
                } else { return Err(error); }
        }};

        println!("{} new videos discovered.", new_videos_count);

        if commandline_options.discover {
            for video in courses[0].files.iter_mut() {
                video.download_state = DownloadState::None;
            }
        }

        if new_videos_count > 0 && !commandline_options.discover {
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
                for (course_index, file_index, error) in failed_downloads {
                    let course = &courses[*course_index];
                    let video = &course.files[*file_index].file;
                    println!("Download of {} failed:", video.metadata);
                    println!("{}", error);
                }
            }

            perform_postprocessing(&courses, &successful_downloads_indices)?;
        }
        save_courses(&commandline_options.state_file, &courses)?;

        continue_next_check = interval.is_some();
    }
    Ok(())
}

#[derive(Debug)]
pub struct CheckForUpdatesError {
    pub new_videos_count: u32,
    pub errors: Vec<GenericError>
}
impl Display for CheckForUpdatesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for CheckForUpdatesError {}

async fn check_for_updates(courses: &mut Vec<Course>, tum_username: &str, tum_password: &str) -> GenericResult<u32> {
    let mut new_videos_count = 0;
    let moodle_auth_cookies = moodle_login(tum_username, tum_password).await?;
    let mut errors = vec![];

    for course in courses {
        match course.course_type {
            CourseType::Moodle => {
                let detection_result = detect_moodle_files(&course.url, moodle_auth_cookies.clone()).await;
                let mut moodle_files = match detection_result {
                    Ok(files) => files,
                    Err(error) => {
                        // First try the downcast then perform it, to not move the error if we have another error type
                        if error.downcast_ref::<MoodleCrawlingError>().is_some() {
                            if let Ok(moodle_crawling_error) = error.downcast::<MoodleCrawlingError>() {
                                errors.extend(moodle_crawling_error.failed_detections);
                                moodle_crawling_error.successful_detections
                            } else { unreachable!() }
                        } else { return Err(error)}
                }};

                // Deduplicate found files and update availability information
                for existing_course_file in &mut course.files {
                    if let Some((i, _)) = moodle_files.iter().enumerate().find(
                            |(_, v)| **v == existing_course_file.file) {
                        moodle_files.remove(i);
                    } else {
                        existing_course_file.available = false;
                    }
                }
                
                // Add newly found videos to `course.files`
                for course_file in moodle_files {
                    let request_download = (course_file.is_video() && course.auto_download_videos_enabled())
                        || (course_file.is_document() && course.auto_download_documents_enabled());
                    let file_download_data = CourseFileDownload {
                        file: course_file,
                        available: true,
                        download_state: if request_download {DownloadState::Requested} else {DownloadState::None},
                        discovery_time: chrono::Utc::now(),
                        download_time: None
                    };
                    course.files.push(file_download_data);
                    new_videos_count += 1;
                }
                
            },
            CourseType::TumLive => todo!(),
            CourseType::GenericWebsite => todo!(),
        }
    }
    if errors.len() == 0 {
        Ok(new_videos_count)
    } else {
        Err(CheckForUpdatesError { new_videos_count, errors }.into())
    }
}

type ProcessDownloadsResult = Result<Vec<(usize, usize)>, 
    (Vec<(usize, usize)>, Vec<(usize, usize, GenericError)>)>;

async fn process_downloads(courses: &mut Vec<Course>, max_parallel_downloads: usize)
        -> ProcessDownloadsResult {
    let mut download_futures = FuturesOrdered::new();

    // Store course and video indices of all, successful and failed downloads
    // s.t. after the loop, when the mutable borrow of courses has ended, the download states can be updated.
    let mut attempted_downloads_indices = vec![];
    let mut download_results = vec![];

    // Iterating over all videos of all courses
    for (i, course) in courses.iter_mut().enumerate() {
        for (j, file) in course.files.iter_mut().enumerate() {

            // If the video is requested to be downloaded:
            if file.download_state == DownloadState::Requested {
                // Mark download as attempted, construct a download future depending on the video type.
                attempted_downloads_indices.push((i, j));
                let download_future: Pin<Box<dyn Future<Output = GenericResult<()>>>> =
                match &file.file.resource {
                    CourseFileResource::Mp4File { url, .. } => {
                        // For mp4 files: identify target filename from url
                        match url.split("/").last() {
                            Some(filename) => {
                                let path = course.video_download_directory.join(filename.to_owned());
                                // Set download state to running and build the download future
                                file.download_state = DownloadState::Running(path.clone());
                                Box::pin(reqwest::get(url)
                                    .err_into::<GenericError>()
                                    .and_then(move |response| download_mp4(response, path)))
                            }
                                // If no filename can be identified: add future indicating this failure
                            None => { Box::pin(async { Err(simple_error!("URL has no '/'").into()) }) }
                        }
                    },
                    CourseFileResource::HlsStream { .. } => todo!(),
                    CourseFileResource::Document { url, .. } => {                        
                        // For documents: identify target filename from url
                        match url.split("/").last() {
                            Some(filename) => {
                                let path = course.file_download_directory.join(filename.to_owned());
                                // Set download state to running and build the download future
                                file.download_state = DownloadState::Running(path.clone());
                                Box::pin(reqwest::get(url)
                                    .err_into::<GenericError>()
                                    .and_then(move |response| download_document(response, path)))
                            }
                                // If no filename can be identified: add future indicating this failure
                            None => { Box::pin(async { Err(simple_error!("URL has no '/'").into()) }) }
                        }
                    }
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
    for (&(course_index, file_index), result) in attempted_downloads_indices.iter().zip(download_results) {
        match result {
            Ok(_) => {
                // If the download was successful: set state to `Completed`
                if let DownloadState::Running(ref path) = courses[course_index].files[file_index].download_state {
                    courses[course_index].files[file_index].download_state = DownloadState::Completed(path.clone());
                    courses[course_index].files[file_index].download_time = Some(chrono::Utc::now());
                    successful_downloads_indices.push((course_index, file_index))
                }
            },
            Err(error) => {
                courses[course_index].files[file_index].download_state = DownloadState::Failed;
                unsuccessful_downloads_indices.push((course_index, file_index, error))
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
        let video = &course.files[video_id];
        for step in &course.video_post_processing_steps {
            perform_postprocessing_step(video, step)?;
        }
    }
    Ok(())
}
