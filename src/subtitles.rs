//! On-demand subtitle discovery. Provider credentials and stream URLs never enter UI models.
use std::{io::Read, path::PathBuf, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Client, StatusCode};
use sha2::{Digest, Sha256};

use crate::{
    fileparser::parse_name,
    putio::{
        self,
        client::ApiError,
        types::{DirectoryNode, PutIoFile},
        PutioClient,
    },
};

pub const MAX_SUBTITLE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum Download {
    Putio(String),
    BsPlayer(String),
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: String,
    pub name: String,
    pub language: String,
    pub language_code: String,
    pub source: String,
    pub format: String,
    pub download: Download,
}

pub struct SearchResult {
    pub candidates: Vec<Candidate>,
    pub note: String,
}

fn cached_file(file_id: u64) -> Option<PutIoFile> {
    fn find(node: &DirectoryNode, id: u64) -> Option<PutIoFile> {
        node.files
            .iter()
            .find(|f| f.id == id)
            .cloned()
            .or_else(|| node.children.iter().find_map(|n| find(n, id)))
    }
    let tree = crate::storage::files_store::FilesStore::load()
        .ok()?
        .read_tree()
        .ok()?;
    find(&tree.root, file_id)
}

pub async fn search(
    client: &PutioClient,
    token: &str,
    file_id: u64,
    original_url: Option<String>,
    progress: impl Fn(&str),
) -> Result<SearchResult> {
    if token.is_empty() {
        bail!("Sign in to put.io to search for subtitles.");
    }
    let file = match cached_file(file_id) {
        Some(file) => file,
        None => putio::subtitles::file(client, token, file_id)
            .await
            .map_err(api_message)?,
    };
    let (languages, listed) = tokio::join!(
        putio::subtitles::languages(client, token),
        putio::subtitles::list(client, token, file_id)
    );
    if matches!(languages, Err(ApiError::Unauthorized)) {
        return Err(api_message(ApiError::Unauthorized));
    }
    let mut languages = languages.unwrap_or_default();
    languages.retain(|l| !l.trim().is_empty());
    if languages.is_empty() {
        languages.push("eng".into());
    }
    let mut putio_failed = false;
    let mut ranked = match listed {
        Ok(subtitles) => subtitles
            .into_iter()
            .filter_map(|sub| {
                let rank = match_score(&file.name, &sub.name)?;
                let source = match sub.source.as_str() {
                    "mkv" => "put.io · Embedded",
                    "folder" => "put.io · Folder",
                    "opensubtitles" | "opensubtitles_rest" => "put.io · OpenSubtitles",
                    _ => "put.io",
                };
                Some((
                    rank + if sub.source == "mkv" { 100 } else { 0 },
                    Candidate {
                        id: format!("putio:{}", sub.key),
                        name: sub.name,
                        language: sub.language,
                        language_code: sub.language_code,
                        source: source.into(),
                        format: "srt".into(),
                        download: Download::Putio(sub.key),
                    },
                ))
            })
            .collect::<Vec<_>>(),
        Err(ApiError::Unauthorized) => return Err(api_message(ApiError::Unauthorized)),
        Err(_) => {
            putio_failed = true;
            Vec::new()
        }
    };
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    let mut seen = std::collections::HashSet::new();
    let candidates: Vec<_> = ranked
        .into_iter()
        .map(|(_, c)| c)
        .filter(|c| seen.insert(c.id.clone()))
        .collect();
    if !candidates.is_empty() {
        return Ok(SearchResult {
            candidates,
            note: String::new(),
        });
    }
    progress("Searching BSPlayer…");
    let fallback = tokio::time::timeout(Duration::from_secs(20), async {
        let url = match original_url {
            Some(url) => url,
            None => putio::stream::resolve_play_url(client, token, file_id)
                .await
                .map_err(api_message)?,
        };
        let candidates = bsplayer_search(&url, file.size, &languages).await?;
        let mut ranked: Vec<_> = candidates
            .into_iter()
            .filter_map(|candidate| {
                match_score(&file.name, &candidate.name).map(|rank| (rank, candidate))
            })
            .collect();
        ranked.sort_by_key(|a| std::cmp::Reverse(a.0));
        Ok::<_, anyhow::Error>(
            ranked
                .into_iter()
                .map(|(_, candidate)| candidate)
                .collect::<Vec<_>>(),
        )
    })
    .await;
    match fallback {
        Ok(Ok(candidates)) if !candidates.is_empty() => Ok(SearchResult {
            candidates,
            note: String::new(),
        }),
        Ok(Ok(_)) if !putio_failed => Ok(SearchResult {
            candidates: Vec::new(),
            note: "No matching subtitles found on put.io or BSPlayer.".into(),
        }),
        _ => Err(anyhow!(if putio_failed {
            "Subtitle services are unavailable. Please try again."
        } else {
            "No matching subtitles on put.io. BSPlayer is unavailable; please try again."
        })),
    }
}

fn api_message(error: ApiError) -> anyhow::Error {
    match error {
        ApiError::Unauthorized => {
            anyhow!("Your put.io session expired. Sign in again to search for subtitles.")
        }
        _ => anyhow!("Couldn't retrieve subtitles from put.io. Please try again."),
    }
}

fn normalized(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Exclude explicit mismatches, retaining unlabelled sidecars for manual selection.
fn match_score(video: &str, subtitle: &str) -> Option<u32> {
    let stem = |name: &str| {
        name.rsplit_once('.')
            .filter(|(_, ext)| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "srt" | "vtt" | "ass" | "ssa"
                )
            })
            .map_or(name.to_string(), |(s, _)| s.to_string())
    };
    let expected = parse_name(video, true);
    let actual = parse_name(&stem(subtitle), true);
    if expected
        .season
        .zip(actual.season)
        .is_some_and(|(a, b)| a != b)
        || (!expected.episodes.is_empty()
            && !actual.episodes.is_empty()
            && expected.episodes != actual.episodes)
        || expected.year.zip(actual.year).is_some_and(|(a, b)| a != b)
    {
        return None;
    }
    let title = normalized(&expected.title);
    let found = normalized(&actual.title);
    // When both identify an episode/year, different titles are a real conflict.
    let identifiable = !actual.episodes.is_empty() || actual.year.is_some();
    if identifiable && !title.is_empty() && !found.is_empty() && title != found {
        return None;
    }
    let mut score = if title == found { 50 } else { 0 };
    let video_stem = std::path::Path::new(video)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(video);
    if normalized(video_stem) == normalized(&stem(subtitle)) {
        score += 200;
    }
    if !expected.episodes.is_empty() && expected.episodes == actual.episodes {
        score += 30;
    }
    if !expected.encoder.is_empty() && expected.encoder.eq_ignore_ascii_case(&actual.encoder) {
        score += 10;
    }
    if !expected.resolution.is_empty() && expected.resolution == actual.resolution {
        score += 5;
    }
    if !expected.quality.is_empty() && expected.quality == actual.quality {
        score += 5;
    }
    Some(score)
}

fn http_client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("PutMPV/1.0")
        .build()?)
}

async fn limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        bail!("Subtitle service returned an error");
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        bail!("Subtitle response is too large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > limit {
            bail!("Subtitle response is too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn range(client: &Client, url: &str, start: u64, size: u64) -> Result<Vec<u8>> {
    let response = client
        .get(url)
        .header("Range", format!("bytes={start}-{}", start + 65535))
        .header("Accept-Encoding", "identity")
        .send()
        .await?;
    let expected = format!("bytes {start}-{}/{size}", start + 65535);
    if response.status() != StatusCode::PARTIAL_CONTENT
        || response
            .headers()
            .get("Content-Range")
            .and_then(|v| v.to_str().ok())
            != Some(expected.as_str())
    {
        bail!("The original video does not support byte ranges");
    }
    let data = limited(response, 65536).await?;
    if data.len() != 65536 {
        bail!("Incomplete video byte range");
    }
    Ok(data)
}

fn movie_hash(size: u64, first: &[u8], last: &[u8]) -> Result<String> {
    if size < 131072 || first.len() != 65536 || last.len() != 65536 {
        bail!("Invalid hash input");
    }
    let hash = first
        .as_chunks::<8>()
        .0
        .iter()
        .chain(last.as_chunks::<8>().0.iter())
        .fold(size, |sum, bytes| {
            sum.wrapping_add(u64::from_le_bytes(*bytes))
        });
    Ok(format!("{hash:016x}"))
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

async fn soap(client: &Client, endpoint: &str, action: &str, params: &str) -> Result<String> {
    let body = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><SOAP-ENV:Envelope xmlns:SOAP-ENV=\"http://schemas.xmlsoap.org/soap/envelope/\" xmlns:SOAP-ENC=\"http://schemas.xmlsoap.org/soap/encoding/\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" xmlns:ns1=\"{endpoint}\"><SOAP-ENV:Body SOAP-ENV:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><ns1:{action}>{params}</ns1:{action}></SOAP-ENV:Body></SOAP-ENV:Envelope>");
    let response = client
        .post(endpoint)
        .header("User-Agent", "BSPlayer/2.x (1022.12360)")
        .header("Content-Type", "text/xml; charset=utf-8")
        .header(
            "SOAPAction",
            format!("\"http://api.bsplayer-subtitles.com/v1.php#{action}\""),
        )
        .body(body)
        .send()
        .await?;
    String::from_utf8(limited(response, MAX_SUBTITLE_BYTES).await?)
        .context("Invalid subtitle response encoding")
}

fn child<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> &'a str {
    node.children()
        .find(|n| n.is_element() && n.tag_name().name() == name)
        .and_then(|n| n.text())
        .unwrap_or("")
}

fn parse_bsplayer(xml: &str, hash: &str, size: u64) -> Result<Vec<Candidate>> {
    let doc = roxmltree::Document::parse(xml)?;
    if !doc
        .descendants()
        .any(|n| n.has_tag_name("status") && n.text() == Some("OK"))
    {
        bail!("BSPlayer search failed");
    }
    let mut results = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in doc.descendants().filter(|n| n.has_tag_name("item")) {
        let id = child(item, "subID");
        let format = child(item, "subFormat")
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if id.is_empty()
            || !seen.insert(id.to_string())
            || !matches!(format.as_str(), "srt" | "ass" | "ssa" | "vtt")
        {
            continue;
        }
        let matched_hash = child(item, "movieHash");
        // BSPlayer uses 0 for an unknown size, including real hash-matched results.
        let matched_size = child(item, "movieSize")
            .parse::<u64>()
            .ok()
            .filter(|size| *size > 0);
        if (!matched_hash.is_empty() && !matched_hash.eq_ignore_ascii_case(hash))
            || matched_size.is_some_and(|s| s != size)
        {
            continue;
        }
        let url = child(item, "subDownloadLink");
        if !safe_subtitle_url(url) {
            continue;
        }
        let name = child(item, "subName");
        results.push(Candidate {
            id: format!("bsplayer:{id}"),
            name: if name.is_empty() {
                format!("Subtitle {id}")
            } else {
                name.into()
            },
            language: match child(item, "subLang") {
                "eng" => "English".into(),
                code => code.to_uppercase(),
            },
            language_code: child(item, "subLang").into(),
            source: "BSPlayer · File match".into(),
            format,
            download: Download::BsPlayer(url.into()),
        });
    }
    Ok(results)
}

fn safe_subtitle_url(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        matches!(u.scheme(), "http" | "https")
            && u.username().is_empty()
            && u.password().is_none()
            && u.host_str().is_some_and(|host| {
                host == "bsplayer-subtitles.com" || host.ends_with(".bsplayer-subtitles.com")
            })
    })
}

async fn bsplayer_search(url: &str, size: u64, languages: &[String]) -> Result<Vec<Candidate>> {
    if size < 131072 {
        bail!("Video too small to identify");
    }
    let client = http_client()?;
    let (first, last) = tokio::try_join!(
        range(&client, url, 0, size),
        range(&client, url, size - 65536, size)
    )?;
    let hash = movie_hash(size, &first, &last)?;
    for host in ["s1", "s5"] {
        let endpoint = format!("http://{host}.api.bsplayer-subtitles.com/v1.php");
        let attempt: Result<Vec<Candidate>> = async {
            let login = soap(&client, &endpoint, "logIn", "<username></username><password></password><AppID>BSPlayer v2.67</AppID>").await?;
            let handle = {
                let doc = roxmltree::Document::parse(&login)?;
                let result = doc.descendants().find(|n| n.has_tag_name("return")).context("No session response")?;
                if child(result, "status") != "OK" { bail!("BSPlayer session failed"); }
                child(result, "data").to_string()
            };
            if handle.is_empty() { bail!("Empty session"); }
            let params = format!("<handle>{}</handle><movieHash>{hash}</movieHash><movieSize>{size}</movieSize><languageId>{}</languageId><imdbId>*</imdbId>", xml_escape(&handle), xml_escape(&languages.join(",")));
            let response = soap(&client, &endpoint, "searchSubtitles", &params).await;
            let _ = tokio::time::timeout(Duration::from_secs(1), soap(&client, &endpoint, "logOut", &format!("<handle>{}</handle>", xml_escape(&handle)))).await;
            parse_bsplayer(&response?, &hash, size)
        }.await;
        if let Ok(results) = attempt {
            return Ok(results);
        }
    }
    bail!("BSPlayer is unavailable")
}

fn validate_content(bytes: &[u8], format: &str) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_SUBTITLE_BYTES {
        bail!("Subtitle file is empty or too large");
    }
    // BSPlayer may supply a legacy text encoding; MPV handles decoding. Reject binary/HTML responses.
    if bytes.contains(&0) {
        bail!("Unsupported binary subtitle file");
    }
    let text = String::from_utf8_lossy(bytes);
    let lower = text.to_ascii_lowercase();
    if lower.contains("<html") || lower.contains("<!doctype html") {
        bail!("The provider returned a web page instead of subtitles");
    }
    let valid = match format {
        "srt" | "vtt" => text
            .lines()
            .any(|l| l.contains("-->") && l.chars().any(|c| c.is_ascii_digit())),
        "ass" | "ssa" => lower.contains("[events]") && lower.contains("dialogue:"),
        _ => false,
    };
    if !valid {
        bail!("The provider returned an invalid subtitle file");
    }
    Ok(())
}

fn decompress(bytes: &[u8]) -> Result<Vec<u8>> {
    let reader: Box<dyn Read + '_> = if bytes.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::GzDecoder::new(bytes))
    } else {
        Box::new(flate2::read::ZlibDecoder::new(bytes))
    };
    let mut output = Vec::new();
    reader
        .take(MAX_SUBTITLE_BYTES as u64 + 1)
        .read_to_end(&mut output)?;
    if output.len() > MAX_SUBTITLE_BYTES {
        bail!("Subtitle file is too large");
    }
    Ok(output)
}

pub async fn download(
    client: &PutioClient,
    token: &str,
    file_id: u64,
    candidate: &Candidate,
) -> Result<PathBuf> {
    let key = format!(
        "{:x}",
        Sha256::digest(format!("{file_id}:{}", candidate.id).as_bytes())
    );
    let dir = crate::storage::config_dir()?.join("subtitles");
    let path = dir.join(format!("{key}.{}", candidate.format));
    if let Ok(metadata) = tokio::fs::metadata(&path).await {
        if metadata.len() <= MAX_SUBTITLE_BYTES as u64 {
            if let Ok(bytes) = tokio::fs::read(&path).await {
                if validate_content(&bytes, &candidate.format).is_ok() {
                    return Ok(path);
                }
            }
        }
    }
    let bytes = match &candidate.download {
        Download::Putio(key) => {
            putio::subtitles::download(client, token, file_id, key, MAX_SUBTITLE_BYTES)
                .await
                .map_err(api_message)?
        }
        Download::BsPlayer(url) => {
            if !safe_subtitle_url(url) {
                bail!("Invalid subtitle download address");
            }
            let response = http_client()?
                .get(url)
                .header("User-Agent", "BSPlayer/2.x (1022.12360)")
                .send()
                .await?;
            decompress(&limited(response, MAX_SUBTITLE_BYTES).await?)?
        }
    };
    validate_content(&bytes, &candidate.format)?;
    tokio::fs::create_dir_all(&dir).await?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let temp = dir.join(format!(".{key}-{nonce}.tmp"));
    tokio::fs::write(&temp, bytes).await?;
    if let Err(error) = tokio::fs::rename(&temp, &path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error.into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn serve_response(status: &str, headers: &str, body: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n{headers}\r\n");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let n = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..n]).to_ascii_lowercase();
            assert!(request.contains("range: bytes=0-65535"));
            assert!(request.contains("accept-encoding: identity"));
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.write_all(&body).await;
        });
        format!("http://{address}/original.mkv")
    }

    #[tokio::test]
    async fn only_hashes_complete_original_byte_ranges() {
        let client = http_client().unwrap();
        let url = serve_response(
            "206 Partial Content",
            "Content-Range: bytes 0-65535/200000\r\nContent-Length: 65536\r\n",
            vec![7; 65536],
        )
        .await;
        assert_eq!(
            range(&client, &url, 0, 200000).await.unwrap(),
            vec![7; 65536]
        );
        // Reject servers that ignore Range before reading a potentially huge video body.
        let url = serve_response("200 OK", "Content-Length: 200000\r\n", Vec::new()).await;
        assert!(range(&client, &url, 0, 200000).await.is_err());
        let url = serve_response(
            "206 Partial Content",
            "Content-Range: bytes 0-65535/999999\r\nContent-Length: 65536\r\n",
            Vec::new(),
        )
        .await;
        assert!(range(&client, &url, 0, 200000).await.is_err());
        let url = serve_response(
            "206 Partial Content",
            "Content-Range: bytes 0-65535/200000\r\nContent-Length: 8\r\n",
            vec![0; 8],
        )
        .await;
        assert!(range(&client, &url, 0, 200000).await.is_err());
    }

    #[test]
    fn rejects_wrong_episodes_and_different_shows() {
        let video = "Manhunt Unabomber S01E07 Lincoln.mkv";
        assert!(match_score(video, "Manhunt Unabomber S01E07 Lincoln.srt").is_some());
        assert!(match_score(video, "Manhunt.Unabomber.S01E07.WEBRip.x264-RARBG.srt").is_some());
        assert!(
            match_score(video, "Manhunt-Unabomber.S01E04.720p.HDTV.x264-CRiMSON.srt").is_none()
        );
        assert!(match_score(
            video,
            "Manhunt.2024.S01E07.ATVP.WEB-DL.DDP5.1.H.264-NTb.en[cc].srt"
        )
        .is_none());
        assert!(match_score(
            video,
            "Land of the Giants (1968) - S01E07 - Manhunt.eng.srt"
        )
        .is_none());
        assert!(match_score("Dune.2021.1080p.mkv", "Dune.1984.srt").is_none());
        assert!(match_score(video, "English.srt").is_some());
        assert!(
            match_score(video, "Manhunt Unabomber S01E07 Lincoln.srt")
                > match_score(video, "Manhunt.Unabomber.S01E07.WEBRip.x264-RARBG.srt")
        );
    }

    #[test]
    fn hashes_little_endian_words_and_wraps() {
        let first = vec![255; 65536];
        let last = vec![0; 65536];
        assert_eq!(
            movie_hash(131072, &first, &last).unwrap(),
            format!("{:016x}", 131072_u64 - 8192)
        );
        assert!(movie_hash(10, &first, &last).is_err());
        assert!(movie_hash(131072, &[0; 8], &last).is_err());
    }

    #[test]
    fn validates_downloads_and_bounded_decompression() {
        use std::io::Write;
        let srt = b"1\n00:00:01,000 --> 00:00:02,000\nHello\n";
        assert!(validate_content(srt, "srt").is_ok());
        assert!(validate_content(b"<html>error --></html>", "srt").is_err());
        assert!(validate_content(b"", "srt").is_err());
        assert!(validate_content(
            b"[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hello",
            "ass"
        )
        .is_ok());
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(srt).unwrap();
        assert_eq!(decompress(&gzip.finish().unwrap()).unwrap(), srt);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&vec![b'a'; MAX_SUBTITLE_BYTES + 1]).unwrap();
        assert!(decompress(&gzip.finish().unwrap()).is_err());
    }

    #[test]
    fn parses_soap_candidates_and_rejects_conflicting_hashes() {
        let xml = r#"<Envelope><return><result><status>OK</status></result><data><item><subID>42</subID><subName>Example.srt</subName><subFormat>srt</subFormat><subLang>eng</subLang><movieHash>abc</movieHash><movieSize>200000</movieSize><subDownloadLink>http://s1.api.bsplayer-subtitles.com/sub.gz</subDownloadLink></item></data></return></Envelope>"#;
        assert_eq!(parse_bsplayer(xml, "abc", 200000).unwrap().len(), 1);
        assert!(parse_bsplayer(xml, "def", 200000).unwrap().is_empty());
        assert!(parse_bsplayer(xml, "abc", 300000).unwrap().is_empty());
        assert_eq!(
            parse_bsplayer(
                &xml.replace("<movieSize>200000</movieSize>", "<movieSize>0</movieSize>"),
                "abc",
                200000
            )
            .unwrap()
            .len(),
            1
        );
        assert!(!safe_subtitle_url("file:///etc/passwd"));
        assert!(!safe_subtitle_url(
            "http://bsplayer-subtitles.com.evil.example/sub"
        ));
    }
}
