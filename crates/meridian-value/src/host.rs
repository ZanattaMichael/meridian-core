//! The one rule for a target host, checked by the core before a plugin sees
//! it and again by every runner that hands it to a tool.
//!
//! A host reaches Meridian from three places: written inline, placed from
//! `--data`, or read from an Infrastructure document's output, which is
//! whatever a tool or a cloud API returned. It then goes to ssh, Bolt,
//! `chef-run`, `salt-ssh`, a Puppet `node` statement and an Ansible
//! inventory. Each of those reads some text as more than a name: ssh reads a
//! leading `-` as an option, an inventory reads a space as the start of a
//! host variable, a manifest reads a quote as the end of the string. So a
//! host is held to what a name can be, and nothing else is passed on.

/// Checks that `host` is something a tool can be pointed at: a host name, an
/// address, or `user@host`, and not an option or a line of configuration.
///
/// What is allowed: letters, digits, and `. - _ : @ [ ] %`. That covers a
/// name, an IPv4 or IPv6 address, an address in brackets with a zone, and a
/// user before an `@`. What is refused: empty text, a leading `-` (an option
/// to ssh and every tool built on it), whitespace, control characters,
/// quotes, and any other punctuation. The error says which.
pub fn check(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("a target host cannot be empty".to_owned());
    }
    if host.starts_with('-') {
        return Err(format!(
            "{host:?} starts with \"-\", which a tool would read as an option and not as a host"
        ));
    }
    for c in host.chars() {
        if c.is_alphanumeric() || ".-_:@[]%".contains(c) {
            continue;
        }
        let what = if c.is_whitespace() {
            "whitespace".to_owned()
        } else if c.is_control() {
            "a control character".to_owned()
        } else {
            format!("{c:?}")
        };
        return Err(format!(
            "{host:?} holds {what}, and a target host can only hold letters, digits, and \
             \". - _ : @ [ ] %\": a host name, an address, or user@host"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn names_addresses_and_users_pass() {
        for host in [
            "web01",
            "web01.example.com",
            "10.0.0.5",
            "::1",
            "fe80::1%eth0",
            "[fe80::1]",
            "deploy@web01.example.com",
            "localhost",
            "münchen.example",
        ] {
            assert_eq!(check(host), Ok(()), "{host}");
        }
    }

    #[test]
    fn options_lines_and_quotes_are_refused() {
        let refused = [
            ("", "empty"),
            ("-oProxyCommand=touch /tmp/pwned", "starts with \"-\""),
            ("web01 ansible_connection=local", "whitespace"),
            ("web01\n[all:vars]", "whitespace"),
            ("web01\u{0}", "a control character"),
            (
                "x' { } exec { 'x': command => 'touch /tmp/pwned' } node 'y",
                "'\\''",
            ),
            ("web01;id", "';'"),
            ("web01$(id)", "'$'"),
            ("web01/path", "'/'"),
        ];
        for (host, says) in refused {
            let why = check(host).expect_err(host);
            assert!(why.contains(says), "{host:?}: {why}");
        }
    }
}
