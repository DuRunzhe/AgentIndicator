#[cfg(target_os = "macos")]
pub fn choose(
    message: &str,
    title: &str,
    buttons: &[&str],
    default: &str,
    cancel: Option<&str>,
) -> Option<String> {
    let buttons = buttons
        .iter()
        .map(|button| format!("\"{}\"", escape(button)))
        .collect::<Vec<_>>()
        .join(", ");
    let cancel = cancel
        .map(|button| format!(" cancel button \"{}\"", escape(button)))
        .unwrap_or_default();
    let script = format!("display dialog \"{}\" with title \"{}\" buttons {{{buttons}}} default button \"{}\"{cancel}", escape(message), escape(title), escape(default));
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .split("button returned:")
        .nth(1)?
        .split([',', '\n'])
        .next()
        .map(|value| value.trim().to_owned())
}

#[cfg(not(target_os = "macos"))]
pub fn choose(_: &str, _: &str, _: &[&str], _: &str, _: Option<&str>) -> Option<String> {
    None
}

pub fn notice(message: &str, title: &str) {
    let _ = choose(message, title, &["确定"], "确定", None);
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes_applescript_strings() {
        assert_eq!(escape("a\\b\"c"), "a\\\\b\\\"c");
    }
}
