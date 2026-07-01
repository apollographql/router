pub fn to_camel_case(kebab: &str) -> String {
    let mut out = String::with_capacity(kebab.len());
    let mut upper_next = false;
    for (i, ch) in kebab.chars().enumerate() {
        if ch == '-' {
            upper_next = true;
            continue;
        }
        if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else if i == 0 {
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

pub fn to_pascal_case(kebab: &str) -> String {
    let mut out = String::with_capacity(kebab.len());
    let mut upper_next = true;
    for ch in kebab.chars() {
        if ch == '-' {
            upper_next = true;
            continue;
        }
        if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_basic() {
        assert_eq!(to_camel_case("get-repository"), "getRepository");
        assert_eq!(to_camel_case("per-page"), "perPage");
        assert_eq!(to_camel_case("user-name"), "userName");
        assert_eq!(to_camel_case("get"), "get");
        assert_eq!(to_camel_case(""), "");
    }

    #[test]
    fn pascal_basic() {
        assert_eq!(to_pascal_case("get-repository"), "GetRepository");
        assert_eq!(to_pascal_case("user"), "User");
        assert_eq!(to_pascal_case("create-or-update-file"), "CreateOrUpdateFile");
    }

    #[test]
    fn initialism_passthrough() {
        // We don't lowercase mid-segment chars, so already-upper input is preserved.
        assert_eq!(to_camel_case("DNS-error"), "dNSError");
        assert_eq!(to_pascal_case("DNS-error"), "DNSError");
    }
}
