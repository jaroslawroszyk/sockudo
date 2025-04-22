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
pub fn data_to_bytes<T: AsRef<str> + serde::Serialize>(data: &[T]) -> usize {
    data.iter()
        .map(|element| {
            // Convert element to string representation
            let string_data = if let Ok(s) = element.as_ref().to_string().parse::<String>() {
                // Element is already a string-like value
                s
            } else {
                // Convert to JSON string representation
                serde_json::to_string(element).unwrap_or_else(|_| String::new())
            };

            // Calculate byte length of string
            string_data.as_bytes().len()
        })
        .sum()
}

pub fn data_to_bytes_flexible(data: Vec<serde_json::Value>) -> usize {
    data.iter().fold(0, |total_bytes, element| {
        let element_str = if element.is_string() {
            // Use string value directly if it's a string
            element.as_str().unwrap_or_default().to_string()
        } else {
            // Convert to JSON string for other types
            serde_json::to_string(element).unwrap_or_default()
        };

        // Add byte length, handling potential encoding errors
        match element_str.as_bytes().len() {
            len => total_bytes + len,
        }
    })
}
