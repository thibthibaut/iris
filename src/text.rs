pub fn cleanup_text(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_whitespace() {
        assert_eq!(cleanup_text(" hello\n\t world  "), "hello world");
    }
}
