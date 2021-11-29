use std::{fmt::Display, path::{PathBuf}};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum CourseFileMetadata {
    TumLiveStream {
        lecture_title: String,
        video_title: String,
        date_time_string: String
    },
    MoodleActivity {
        lecture_title: String,
        section_title: String,
        activity_title: String
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum CourseFileResource {
    Mp4File {
        url: String
    },
    HlsStream {
        main_m3u8_url: String
    },
    Document {
        url: String,
        file_extension: Option<String>
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct CourseFile {
    pub resource: CourseFileResource,
    pub metadata: CourseFileMetadata
}

impl Display for CourseFileMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CourseFileMetadata::TumLiveStream { lecture_title, video_title: title, .. }
            | CourseFileMetadata::MoodleActivity { lecture_title, activity_title: title, .. }  => {
                write!(f, "{} - {}", lecture_title, title)
            }
        }
    }
}

impl CourseFile {
    pub fn is_video(&self) -> bool {
        match self.resource {
            CourseFileResource::Mp4File { .. } |
            CourseFileResource::HlsStream { .. } => true,
            CourseFileResource::Document { .. } => false
        }
    }

    pub fn is_document(&self) -> bool {
        match self.resource {
            CourseFileResource::Mp4File { .. } |
            CourseFileResource::HlsStream { .. } => false,
            CourseFileResource::Document { .. } => true
        }
    }
}

/// For now: Two videos are considered equal if they point to the same resource, ignoring the metadata
impl PartialEq for CourseFile {
    fn eq(&self, other: &Self) -> bool {
        self.resource == other.resource
    }
}

#[derive(PartialEq, Serialize, Deserialize)]
pub enum AutoDownloadMode {
    None,
    Videos,
    Documents,
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
    pub files: Vec<CourseFileDownload<CourseFile>>,
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

    pub fn auto_download_documents_enabled(&self) -> bool {
        match self.auto_download_mode {
            AutoDownloadMode::Documents | AutoDownloadMode::All => true,
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
    FfmpegReencode { target_fps: u32 }
}
