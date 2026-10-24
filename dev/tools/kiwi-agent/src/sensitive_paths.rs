pub fn excluded_component(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    [
        "account",
        "credential",
        "secret",
        "privatekey",
        "private_key",
        "private-key",
        "id_rsa",
        "id_ed25519",
    ]
    .iter()
    .any(|part| value.contains(part))
        || matches!(
            value.as_str(),
            ".aws"
                | ".ssh"
                | ".git"
                | ".env"
                | ".netrc"
                | ".npmrc"
                | ".pypirc"
                | ".git-credentials"
                | "auth.json"
                | "auth.toml"
                | "auth.yaml"
                | "token.json"
                | "tokens.json"
                | "api-key.json"
                | "api_key.json"
        )
        || value.starts_with(".env.")
        || [".pem", ".key", ".p12", ".pfx", ".jks", ".keystore", ".env"]
            .iter()
            .any(|extension| value.ends_with(extension))
}
