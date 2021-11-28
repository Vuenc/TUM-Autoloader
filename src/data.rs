use std::path::{PathBuf};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CourseVideo {
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

impl CourseVideo {
    pub fn url(&self) -> &str {
        match self {
            CourseVideo::TumLiveStream { url, ..} => url,
            CourseVideo::MoodleVideoFile { url, ..} => url,
            CourseVideo::PanoptoVideoFile { url, ..} => url,
        }
    }

    pub fn video_title(&self) -> &str {
        match self {
            CourseVideo::TumLiveStream { video_title, ..} => video_title,
            CourseVideo::MoodleVideoFile { video_title, ..} => video_title,
            CourseVideo::PanoptoVideoFile { video_title, ..} => video_title,
        }
    }
}

#[derive(PartialEq, Serialize, Deserialize)]
pub enum AutoDownloadMode {
    None,
    Videos,
    Files,
    All,
}


#[derive(PartialEq, Debug, Serialize, Deserialize)]
pub enum DownloadState {
    None,
    Requested,
    Running(PathBuf),
    Completed(PathBuf),
    Failed
}

#[derive(PartialEq, Serialize, Deserialize)]
pub enum CourseType {
    Moodle,
    TumLive,
    GenericWebsite
}

#[derive(Serialize, Deserialize)]
pub struct Course {
    // id: i32,
    pub url: String,
    pub name: String,
    pub course_type: CourseType,
    pub video_download_directory: PathBuf,
    pub file_download_directory: PathBuf,
    pub auto_download_mode: AutoDownloadMode,
    pub videos: Vec<CourseFileDownload<CourseVideo>>,
    pub max_keep_days_videos: Option<i32>,
    pub max_keep_videos: Option<i32>,
    pub video_post_processing_steps: Vec<PostprocessingStep>
    /* 
    id
    site url, course name, download directory, re-check interval (seconds), 
    auto download (videos / files / all / none), delete auto downloaded after x days, keep at most y auto downloaded,
    download_videos, download_other_files,
    course_file_downloads
    */
}

impl Course {
    pub fn auto_download_videos_enabled(&self) -> bool {
        match self.auto_download_mode {
            AutoDownloadMode::Videos | AutoDownloadMode::All => true,
            _ => false
        }
    }

    pub fn auto_download_files_enabled(&self) -> bool {
        match self.auto_download_mode {
            AutoDownloadMode::Files | AutoDownloadMode::All => true,
            _ => false
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CourseFileDownload<T> {
    // id: i32,
    pub file: T,
    pub available: bool,
    pub download_state: DownloadState,
    pub discovery_time: chrono::DateTime<chrono::Utc>,
    pub download_time: Option<chrono::DateTime<chrono::Utc>>,
    /*
    id
    download state (none / requested / running / completed), dowload datetime, file (i.e. the CourseVideo struct), path
    */
}

#[derive(PartialEq, Serialize, Deserialize)]
pub enum PostprocessingStep {
    FfmpegReencode { target_fps: i32 }
}
