//! Just enough XML for Wayland protocol files: elements, attributes, text,
//! comments, and the XML declaration. No DTDs, namespaces, or CDATA. A
//! protocol file with anything else is refused rather than half-read.

use std::collections::BTreeMap;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attributes: BTreeMap<String, String>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.get(name).map(String::as_str)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |child| child.name == name)
    }
}

pub fn parse(source: &str) -> Result<Element, String> {
    let mut parser = Parser { rest: source, line: 1 };
    parser.skip_prolog()?;
    let root = parser.element()?;
    parser.skip_misc()?;
    if !parser.rest.is_empty() {
        return Err(parser.error("content after the root element"));
    }
    Ok(root)
}

struct Parser<'a> {
    rest: &'a str,
    line: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, what: &str) -> String {
        format!("line {}: {what}", self.line)
    }

    fn advance(&mut self, bytes: usize) -> &'a str {
        let (taken, rest) = self.rest.split_at(bytes);
        self.line += taken.matches('\n').count();
        self.rest = rest;
        taken
    }

    fn skip_whitespace(&mut self) {
        let skip = self.rest.len() - self.rest.trim_start().len();
        self.advance(skip);
    }

    fn skip_until(&mut self, end: &str) -> Result<(), String> {
        let at = self
            .rest
            .find(end)
            .ok_or_else(|| self.error(&format!("no closing {end}")))?;
        self.advance(at + end.len());
        Ok(())
    }

    /// Whitespace and comments, which may come before and after the root.
    fn skip_misc(&mut self) -> Result<(), String> {
        loop {
            self.skip_whitespace();
            if self.rest.starts_with("<!--") {
                self.skip_until("-->")?;
            } else {
                return Ok(());
            }
        }
    }

    fn skip_prolog(&mut self) -> Result<(), String> {
        self.skip_misc()?;
        if self.rest.starts_with("<?xml") {
            self.skip_until("?>")?;
        }
        self.skip_misc()
    }

    fn name(&mut self) -> Result<String, String> {
        let length = self
            .rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == ':' || c == '.'))
            .unwrap_or(self.rest.len());
        if length == 0 {
            return Err(self.error("expected a name"));
        }
        Ok(self.advance(length).to_string())
    }

    fn element(&mut self) -> Result<Element, String> {
        if !self.rest.starts_with('<') {
            return Err(self.error("expected an element"));
        }
        self.advance(1);
        let mut element = Element {
            name: self.name()?,
            ..Default::default()
        };
        loop {
            self.skip_whitespace();
            if self.rest.starts_with("/>") {
                self.advance(2);
                return Ok(element);
            }
            if self.rest.starts_with('>') {
                self.advance(1);
                break;
            }
            let name = self.name()?;
            self.skip_whitespace();
            if !self.rest.starts_with('=') {
                return Err(self.error("expected = after an attribute name"));
            }
            self.advance(1);
            self.skip_whitespace();
            let quote = self.rest.chars().next().filter(|c| *c == '"' || *c == '\'');
            let quote = quote.ok_or_else(|| self.error("expected a quoted attribute value"))?;
            self.advance(1);
            let end = self
                .rest
                .find(quote)
                .ok_or_else(|| self.error("unterminated attribute"))?;
            let value = unescape(self.advance(end)).map_err(|e| self.error(&e))?;
            self.advance(1);
            if element.attributes.insert(name.clone(), value).is_some() {
                return Err(self.error(&format!("attribute {name} given twice")));
            }
        }
        loop {
            if self.rest.starts_with("</") {
                self.advance(2);
                let closing = self.name()?;
                if closing != element.name {
                    return Err(self.error(&format!("</{closing}> closes <{}>", element.name)));
                }
                self.skip_whitespace();
                if !self.rest.starts_with('>') {
                    return Err(self.error("expected > to end a closing tag"));
                }
                self.advance(1);
                return Ok(element);
            }
            if self.rest.starts_with("<!--") {
                self.skip_until("-->")?;
            } else if self.rest.starts_with("<![CDATA[") || self.rest.starts_with("<!") || self.rest.starts_with("<?") {
                return Err(self.error("markup that protocol files do not use"));
            } else if self.rest.starts_with('<') {
                element.children.push(self.element()?);
            } else if self.rest.is_empty() {
                return Err(self.error(&format!("<{}> is never closed", element.name)));
            } else {
                let end = self.rest.find('<').unwrap_or(self.rest.len());
                let text = unescape(self.advance(end)).map_err(|e| self.error(&e))?;
                element.text.push_str(&text);
            }
        }
    }
}

fn unescape(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest.find(';').ok_or("an & with no ;")?;
        let entity = &rest[1..end];
        match entity {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if entity.starts_with("#x") => {
                let code = u32::from_str_radix(&entity[2..], 16).map_err(|_| format!("bad entity &{entity};"))?;
                out.push(char::from_u32(code).ok_or(format!("bad entity &{entity};"))?);
            }
            _ if entity.starts_with('#') => {
                let code: u32 = entity[1..].parse().map_err(|_| format!("bad entity &{entity};"))?;
                out.push(char::from_u32(code).ok_or(format!("bad entity &{entity};"))?);
            }
            _ => return Err(format!("unknown entity &{entity};")),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_attributes_text_and_comments() {
        let root = parse(
            r#"<?xml version="1.0"?>
            <!-- a comment -->
            <protocol name="p">
              <copyright>A &amp; B &lt;c&gt;</copyright>
              <interface name="x" version='2'><request name="r"/></interface>
            </protocol>"#,
        )
        .unwrap();
        assert_eq!(root.name, "protocol");
        assert_eq!(root.attribute("name"), Some("p"));
        assert_eq!(root.children_named("copyright").next().unwrap().text, "A & B <c>");
        let interface = root.children_named("interface").next().unwrap();
        assert_eq!(interface.attribute("version"), Some("2"));
        assert_eq!(interface.children[0].name, "request");
    }

    #[test]
    fn what_a_protocol_file_does_not_use_is_refused() {
        for bad in [
            "<a><b></a>",
            "<a>",
            "<a x=1/>",
            "<a x='1' x='2'/>",
            "<a><![CDATA[x]]></a>",
            "<a>&nbsp;</a>",
            "<a/><b/>",
        ] {
            assert!(parse(bad).is_err(), "{bad} was accepted");
        }
    }
}
