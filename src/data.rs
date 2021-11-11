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