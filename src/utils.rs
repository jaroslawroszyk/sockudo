use regex::Regex;
pub fn is_cache_channel(channel: &str) -> bool {
    let caching_channel_patterns: Vec<String> = vec![
        "cache-*".to_string(),
        "private-cache-*".to_string(),
        "private-encrypted-cache-*".to_string(),
        "presence-cache-*".to_string(),
    ];
    let mut ok = false;
    for pattern in caching_channel_patterns {
        let regex = Regex::new(&pattern.replace("*", ".*")).unwrap();
        if regex.is_match(channel) {
            ok = true;
        }
    }
    ok
}
