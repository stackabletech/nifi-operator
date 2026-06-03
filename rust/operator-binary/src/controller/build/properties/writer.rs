//! Writer for Java `.properties` files.
//!
//! Vendored from the `product-config` crate's `writer` module so the operator no
//! longer depends on `product-config` for rendering.

use std::io::Write;

use java_properties::{PropertiesError, PropertiesWriter};
use snafu::{ResultExt, Snafu};

#[derive(Debug, Snafu)]
pub enum PropertiesWriterError {
    #[snafu(display("failed to create properties file"))]
    Properties { source: PropertiesError },

    #[snafu(display("failed to convert properties file byte array to UTF-8"))]
    FromUtf8 { source: std::string::FromUtf8Error },
}

/// Creates a common Java properties file string in the format:
/// `property_1=value_1\nproperty_2=value_2\n`.
pub fn to_java_properties_string<'a, T>(properties: T) -> Result<String, PropertiesWriterError>
where
    T: Iterator<Item = (&'a String, &'a Option<String>)>,
{
    let mut output = Vec::new();
    write_java_properties(&mut output, properties)?;
    String::from_utf8(output).context(FromUtf8Snafu)
}

/// Writes Java properties to the given writer. A `None` value is written as an
/// empty value (`key=`).
fn write_java_properties<'a, W, T>(writer: W, properties: T) -> Result<(), PropertiesWriterError>
where
    W: Write,
    T: Iterator<Item = (&'a String, &'a Option<String>)>,
{
    let mut writer = PropertiesWriter::new(writer);
    for (k, v) in properties {
        let property_value = v.as_deref().unwrap_or_default();
        writer.write(k, property_value).context(PropertiesSnafu)?;
    }
    writer.flush().context(PropertiesSnafu)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn props(pairs: &[(&str, Option<&str>)]) -> String {
        let map: BTreeMap<String, Option<String>> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
            .collect();
        to_java_properties_string(map.iter()).unwrap()
    }

    #[test]
    fn java_properties_renders_key_value() {
        assert_eq!(props(&[("a", Some("1")), ("b", Some("2"))]), "a=1\nb=2\n");
    }

    #[test]
    fn java_properties_renders_none_as_empty() {
        assert_eq!(props(&[("none", None)]), "none=\n");
    }

    #[test]
    fn java_properties_escapes_colon_in_value() {
        assert_eq!(
            props(&[("url", Some("file://this/location/file.abc"))]),
            "url=file\\://this/location/file.abc\n"
        );
    }
}
