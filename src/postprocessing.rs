use crate::{GenericResult, data::{CourseFileDownload, CourseFile, DownloadState, PostprocessingStep}};
use std::process::{Command, Stdio};
use tempfile;
use simple_error::simple_error;


pub fn perform_postprocessing_step(video: &CourseFileDownload<CourseFile>, step: &PostprocessingStep) -> GenericResult<()> {
    match step {
        PostprocessingStep::FfmpegReencode { target_fps } => {
            ffmpeg_reencode(video, *target_fps)?;
        }
    }
    Ok(())
}

fn ffmpeg_reencode(video: &CourseFileDownload<CourseFile>, target_fps: u32) -> GenericResult<()> {
    let input_path = if let DownloadState::Completed(path) = &video.download_state {
        path
    } else {
        return Err(simple_error!("Could not postprocess: Video download is not completed.").into());
    };
    let input_path_str = input_path.to_str().ok_or(simple_error!("Could not postprocess: Non-UTF8 path not supported."))?;
    let output_dir = tempfile::tempdir()?;
    let output_filename = input_path.file_name().ok_or(simple_error!("Could not postprocess: No filename found in input path."))?
        .to_str().ok_or(simple_error!("Could not postprocess: Non-UTF8 path not supported."))?;
    let output_path_str = output_dir.path().join(output_filename)
        .to_str().ok_or(simple_error!("Could not postprocess: Non-UTF8 path not supported."))?.to_owned();
    let output_status = Command::new("ffmpeg")
        .args(["-i", input_path_str, 
            "-filter:v",
            &format!("fps=fps={}", target_fps),
            "-codec:v", "libx264",
            "-b:v", "200k",
            "-maxrate:v", "200k",
            "-bufsize:v", "20M",
            "-threads", "1",
            &output_path_str])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?
        .wait();
    if !output_status?.success() {
        return Err(simple_error!("Postprocessing failed: ffmpeg returned non-zero status code.").into());
    }
    std::fs::copy(output_path_str, input_path_str)?;
    Ok(())
}
