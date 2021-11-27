use std::{fs, io::BufWriter};
use std::path::{PathBuf};
use reqwest;
use std::{fs::File, io::Write, env::temp_dir};
use futures::{self, TryFutureExt, stream::{FuturesUnordered, StreamExt}};

use crate::GenericResult;

pub async fn download_mp4(resp: reqwest::Response, path: PathBuf) -> GenericResult<()> {
    // let resp = resp?; Result<reqwest::Response, reqwest::Error>
    // let filename = resp.url().to_string().split("/").last().ok_or(simple_error!("URL has no '/'"))?.to_owned();
    // dbg!(&filename);
    // if !filename.ends_with("mp4") {
    //     return Ok(())
    // }
    // let path = Path::new("../../Studium/TUM Recordings/").join(filename);
    let mut writer = BufWriter::new(File::create(path)?);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        writer.write(&chunk?)?;
    }
    Ok(())
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
