use std::{fmt::Display, path::{Path, PathBuf}, pin::Pin, sync::Arc};

use futures::{Future, StreamExt, TryFutureExt, stream::{FuturesOrdered}};
use tum_autoloader::{GenericError, GenericResult, data::CourseFileResource, download::{download_mp4, download_document},
    moodle::{MoodleCrawlingError, detect_moodle_files, moodle_login},
    tum_live::{tum_live_login, detect_tum_live_videos}};
use simple_error::simple_error;
use tum_autoloader::data::{Course, CourseFileDownload, CourseType, DownloadState, AutoDownloadMode, PostprocessingStep};
use serde_json;
use tum_autoloader::postprocessing::perform_postprocessing_step;
use structopt::StructOpt;
use urlencoding;

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
        let moodle_auth_cookies = moodle_login(&username, &password).await?;

        let check_for_updates_result = check_for_updates(&mut courses, moodle_auth_cookies.clone()).await;
        let (new_videos_count, new_documents_count) = match check_for_updates_result {
            Ok(count) => count,
            Err(error) => {
                if error.downcast_ref::<CheckForUpdatesError>().is_some() {
                    if let Ok(check_for_updates_error) = error.downcast::<CheckForUpdatesError>() {
                        println!("Errors occured while checking for updates.");
                        for error in check_for_updates_error.errors {
                            println!("{}", error);
                        }
                        (check_for_updates_error.new_videos_count, check_for_updates_error.new_documents_count)
                    } else { unreachable!() }
                } else { return Err(error); }
        }};

        println!("{} new videos and {} new documents discovered.", new_videos_count, new_documents_count);

        if commandline_options.discover {
            for video in courses[0].files.iter_mut() {
                video.download_state = DownloadState::None;
            }
        }

        if new_videos_count + new_documents_count > 0 && !commandline_options.discover {
            let downloads_result = process_downloads(&mut courses, 1, moodle_auth_cookies.clone()).await;

            // let (new_videos_count, new_documents_count) = match check_for_updates_result {
            //     Ok(count) => count,
            //     Err(error) => {
            //         if error.downcast_ref::<CheckForUpdatesError>().is_some() {
            //             if let Ok(check_for_updates_error) = error.downcast::<CheckForUpdatesError>() {
            //                 println!("Errors occured while checking for updates.");
            //                 for error in check_for_updates_error.errors {
            //                     println!("{}", error);
            //                 }
            //                 (check_for_updates_error.new_videos_count, check_for_updates_error.new_documents_count)
            //             } else { unreachable!() }
            //         } else { return Err(error); }
            // }};

            let (successful_downloads_indices, failed_downloads) = match downloads_result {
                Ok(successful_downloads_indices) => (successful_downloads_indices, vec![]),
                Err(error) => {
                    if error.downcast_ref::<ProcessDownloadsError>().is_some() {
                        if let Ok(process_downloads_error) = error.downcast::<ProcessDownloadsError>() {
                            (process_downloads_error.successful_downloads_indices, process_downloads_error.unsuccessful_downloads_indices)
                        } else { unreachable!() }
                    } else { return Err(error); }
                }
            };
            println!("Downloaded {} new videos.", successful_downloads_indices.len());
            if failed_downloads.len() > 0 {
                println!("Failed to download {} videos.", failed_downloads.len());
                for (course_index, file_index, error) in failed_downloads {
                    let course = &courses[course_index];
                    let video = &course.files[file_index].file;
                    println!("Download of {} failed:", video.metadata);
                    println!("{}", error);
                }
            }

            let state_file_path = commandline_options.state_file.clone();
            save_courses(&commandline_options.state_file, &courses)?;
            perform_postprocessing(&mut courses, 
                |updated_courses| save_courses(&state_file_path, updated_courses))?;
        }
        save_courses(&commandline_options.state_file, &courses)?;

        continue_next_check = interval.is_some();
    }
    Ok(())
}

#[derive(Debug)]
pub struct CheckForUpdatesError {
    pub new_videos_count: u32,
    pub new_documents_count: u32,
    pub errors: Vec<GenericError>
}
impl Display for CheckForUpdatesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for CheckForUpdatesError {}

async fn check_for_updates(courses: &mut Vec<Course>,
        moodle_auth_cookies: Arc<reqwest_cookie_store::CookieStoreMutex>) -> GenericResult<(u32, u32)> {
    let mut new_videos_count = 0;
    let mut new_documents_count = 0;
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
                    if course_file.is_document() { new_documents_count  += 1; }
                    else if course_file.is_video() { new_videos_count += 1; }

                    let file_download_data = CourseFileDownload {
                        file: course_file,
                        available: true,
                        download_state: if request_download {DownloadState::Requested} else {DownloadState::None},
                        discovery_time: chrono::Utc::now(),
                        download_time: None
                    };
                    course.files.push(file_download_data);
                }
                
            },
            CourseType::TumLive => todo!(),
            CourseType::GenericWebsite => todo!(),
        }
    }
    if errors.len() == 0 {
        Ok((new_videos_count, new_documents_count))
    } else {
        Err(CheckForUpdatesError { new_videos_count, new_documents_count, errors }.into())
    }
}

#[derive(Debug)]
pub struct ProcessDownloadsError {
    pub successful_downloads_indices: Vec<(usize, usize)>,
    pub unsuccessful_downloads_indices: Vec<(usize, usize, GenericError)>
}
impl Display for ProcessDownloadsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}
impl std::error::Error for ProcessDownloadsError {}

async fn process_downloads(courses: &mut Vec<Course>, max_parallel_downloads: usize, 
        moodle_auth_cookies: Arc<reqwest_cookie_store::CookieStoreMutex>) -> GenericResult<Vec<(usize, usize)>> {
    let mut download_futures = FuturesOrdered::new();

    let client = reqwest::Client::builder()
        .cookie_provider(moodle_auth_cookies.clone())
        .build()?;

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
                                let decoded_filename = urlencoding::decode(filename).unwrap().to_string();
                                let path = course.video_download_directory.join(decoded_filename);
                                // Set download state to running and build the download future
                                file.download_state = DownloadState::Running(path.clone());
                                Box::pin(client.get(url).send()
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
                                let decoded_filename = urlencoding::decode(filename).unwrap().to_string();
                                let path = course.file_download_directory.join(decoded_filename);
                                // Set download state to running and build the download future
                                file.download_state = DownloadState::Running(path.clone());
                                Box::pin(client.get(url).send()
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
                let course = &mut courses[course_index];
                let file = &mut course.files[file_index];
                // If the download was successful: set state to `Completed`
                if let DownloadState::Running(ref path) = file.download_state {
                    let needs_postprocessing = !course.video_post_processing_steps.is_empty() && file.file.is_video();
                    let new_state = if needs_postprocessing { DownloadState::PostprocessingPending } 
                        else { DownloadState::Completed }(path.clone());
                    courses[course_index].files[file_index].download_state = new_state;
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
        return Err(ProcessDownloadsError { successful_downloads_indices, unsuccessful_downloads_indices }.into())
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

fn perform_postprocessing<F>(courses: &mut Vec<Course>,
    progress_closure: F) -> GenericResult<()> 
    where F: Fn(&Vec<Course>) -> GenericResult<()>
{
    for i in 0..courses.len() {
        for j in 0..courses[i].files.len() {
            let course = &mut courses[i];
            let file = &mut course.files[j];
            // TODO: check that video_post_processing_steps.is_empty() 
            if let DownloadState::PostprocessingPending(ref path) = file.download_state {
                if file.file.is_video() {
                    for step in &course.video_post_processing_steps {
                        perform_postprocessing_step(file, step)?;
                    }
                    file.download_state = DownloadState::Completed(path.clone());
                    progress_closure(courses)?;
                }
            }
        }
    }
    Ok(())
}
