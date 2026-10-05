//! Blocking capture-worker entry point. Rebuilding captured imports never calls this reader.
use std::time::Duration;
use wes_engine::imports::ImportError;

pub fn validate_url(location: &str) -> Result<reqwest::Url, ImportError> {
    let url = reqwest::Url::parse(location).map_err(|_| ImportError::InvalidRecipe)?;
    if !(location.starts_with("http://") || location.starts_with("https://"))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || location.len() > 4096
        || location.chars().any(char::is_control)
    {
        return Err(ImportError::InvalidRecipe);
    }
    Ok(url)
}

pub fn read_url(location: &str, max_bytes: usize) -> Result<String, ImportError> {
    let url = validate_url(location)?;
    let limit = max_bytes.min(crate::api_library::max_source());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ImportError::Unavailable)?
        .block_on(async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| ImportError::Unavailable)?;
            let mut response = client
                .get(url)
                .send()
                .await
                .map_err(|_| ImportError::Unavailable)?;
            if !response.status().is_success() {
                return Err(ImportError::Unavailable);
            }
            if response
                .content_length()
                .is_some_and(|size| size > limit as u64)
            {
                return Err(ImportError::Capacity);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| ImportError::Unavailable)?
            {
                if chunk.len() > limit.saturating_sub(bytes.len()) {
                    return Err(ImportError::Capacity);
                }
                bytes.extend_from_slice(&chunk);
            }
            String::from_utf8(bytes).map_err(|_| ImportError::InvalidRecipe)
        })
}
