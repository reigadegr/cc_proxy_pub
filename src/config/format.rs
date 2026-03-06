use taplo::formatter;

/// 格式化 TOML 内容
///
/// 使用统一的缩进风格（4个空格）格式化输入的TOML字符串
pub fn format_toml(input: &str) -> String {
    let options = formatter::Options {
        indent_string: "    ".to_string(),
        ..Default::default()
    };
    formatter::format(input, options)
}

#[cfg(test)]
mod tests {
    use super::format_toml;

    #[test]
    fn normalizes_basic_spacing() {
        let input = r#"log_req_body=false
log_res_body =false
[[upstream]]
endpoint= "https://example.com"
model="m"
api_keys=["k1","k2"]
"#;

        let output = format_toml(input);

        assert!(output.contains("log_req_body = false"));
        assert!(output.contains("log_res_body = false"));
        assert!(output.contains("endpoint = \"https://example.com\""));
        assert!(output.contains("model = \"m\""));
        assert!(output.contains("api_keys = [\"k1\", \"k2\"]"));
    }
}
