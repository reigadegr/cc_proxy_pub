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
