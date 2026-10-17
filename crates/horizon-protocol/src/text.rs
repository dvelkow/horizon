//! Chat components as sent on the network (ComponentSerialization in NBT form)
//! flattened to display strings, with `translate` keys resolved through a
//! language table (en_us.json) and Java `String.format` style arguments.
#[cfg(target_arch = "wasm32")]
use horizon_platform as std;
use horizon_world::nbt::{JavaString, Tag};
use std::collections::HashMap;

/// A language table: translation key -> pattern.
pub type Language = HashMap<String, String>;

fn string(tag: &Tag) -> Option<String> {
    match tag {
        Tag::String(s) => Some(s.to_string_lossy()),
        _ => None,
    }
}
fn get<'t>(tag: &'t Tag, key: &str) -> Option<&'t Tag> {
    match tag {
        Tag::Compound(map) => map.get(&JavaString::from(key)),
        _ => None,
    }
}

/// Flatten a component to text.
pub fn plain(tag: &Tag, lang: &Language) -> String {
    let mut out = String::new();
    append(tag, lang, &mut out, 0);
    out
}
fn append(tag: &Tag, lang: &Language, out: &mut String, depth: usize) {
    if depth > 32 {
        return;
    }
    match tag {
        Tag::String(s) => out.push_str(&s.to_string_lossy()),
        Tag::Byte(v) => out.push_str(&v.to_string()),
        Tag::Short(v) => out.push_str(&v.to_string()),
        Tag::Int(v) => out.push_str(&v.to_string()),
        Tag::Long(v) => out.push_str(&v.to_string()),
        Tag::Float(v) => out.push_str(&f32::from_bits(*v).to_string()),
        Tag::Double(v) => out.push_str(&f64::from_bits(*v).to_string()),
        // A list is its first element with the rest appended as siblings.
        Tag::List(items) => {
            for item in items {
                append(item, lang, out, depth + 1);
            }
        }
        Tag::Compound(_) => {
            if let Some(text) = get(tag, "text").and_then(string) {
                out.push_str(&text);
            } else if let Some(value) = get(tag, "") {
                // NbtOps' wrapper for a bare value in a mixed list (a
                // translation's number and string arguments).
                append(value, lang, out, depth + 1);
            } else if let Some(key) = get(tag, "translate").and_then(string) {
                let args: Vec<String> = match get(tag, "with") {
                    Some(Tag::List(args)) => args
                        .iter()
                        .map(|a| {
                            let mut s = String::new();
                            append(a, lang, &mut s, depth + 1);
                            s
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let pattern = lang
                    .get(&key)
                    .cloned()
                    .or_else(|| get(tag, "fallback").and_then(string))
                    .unwrap_or_else(|| key.clone());
                out.push_str(&format_java(&pattern, &args));
            } else if let Some(key) = get(tag, "keybind").and_then(string) {
                out.push_str(lang.get(&key).map_or(key.as_str(), String::as_str));
            } else if let Some(selector) = get(tag, "selector").and_then(string) {
                out.push_str(&selector);
            } else if let Some(score) = get(tag, "score") {
                if let Some(name) = get(score, "name").and_then(string) {
                    out.push_str(&name);
                }
            }
            if let Some(Tag::List(extra)) = get(tag, "extra") {
                for e in extra {
                    append(e, lang, out, depth + 1);
                }
            }
        }
        _ => {}
    }
}

/// The style a component and its children draw in, as legacy `§` codes.
#[derive(Clone, Debug, Default, PartialEq)]
struct Legacy {
    color: Option<char>,
    bold: bool,
    italic: bool,
    underlined: bool,
    strikethrough: bool,
    obfuscated: bool,
}
impl Legacy {
    /// This style with `tag`'s own fields over it (Style.applyTo).
    fn over(&self, tag: &Tag) -> Self {
        let mut s = self.clone();
        if let Some(c) = get(tag, "color").and_then(string).and_then(|c| color_code(&c)) {
            s.color = Some(c);
        }
        let flag = |key: &str, now: &mut bool| match get(tag, key) {
            Some(Tag::Byte(v)) => *now = *v != 0,
            Some(Tag::Int(v)) => *now = *v != 0,
            _ => {}
        };
        flag("bold", &mut s.bold);
        flag("italic", &mut s.italic);
        flag("underlined", &mut s.underlined);
        flag("strikethrough", &mut s.strikethrough);
        flag("obfuscated", &mut s.obfuscated);
        s
    }
    /// Codes that switch to this style from any other: a reset first.
    fn codes(&self) -> String {
        let mut out = String::from("§r");
        if let Some(c) = self.color {
            out.push('§');
            out.push(c);
        }
        for (on, c) in [
            (self.obfuscated, 'k'),
            (self.bold, 'l'),
            (self.strikethrough, 'm'),
            (self.underlined, 'n'),
            (self.italic, 'o'),
        ] {
            if on {
                out.push('§');
                out.push(c);
            }
        }
        out
    }
}

/// ChatFormatting's code for a text colour name, or the nearest of the 16
/// for a `#RRGGBB` one.
fn color_code(name: &str) -> Option<char> {
    const COLORS: [(&str, char, u32); 16] = [
        ("black", '0', 0x000000),
        ("dark_blue", '1', 0x0000AA),
        ("dark_green", '2', 0x00AA00),
        ("dark_aqua", '3', 0x00AAAA),
        ("dark_red", '4', 0xAA0000),
        ("dark_purple", '5', 0xAA00AA),
        ("gold", '6', 0xFFAA00),
        ("gray", '7', 0xAAAAAA),
        ("dark_gray", '8', 0x555555),
        ("blue", '9', 0x5555FF),
        ("green", 'a', 0x55FF55),
        ("aqua", 'b', 0x55FFFF),
        ("red", 'c', 0xFF5555),
        ("light_purple", 'd', 0xFF55FF),
        ("yellow", 'e', 0xFFFF55),
        ("white", 'f', 0xFFFFFF),
    ];
    if let Some(&(_, c, _)) = COLORS.iter().find(|(n, ..)| *n == name) {
        return Some(c);
    }
    let rgb = u32::from_str_radix(name.strip_prefix('#')?, 16).ok()?;
    let channel = |v: u32, shift: u32| ((v >> shift) & 0xFF) as i32;
    COLORS
        .iter()
        .min_by_key(|(_, _, c)| {
            [16, 8, 0]
                .iter()
                .map(|&sh| (channel(rgb, sh) - channel(*c, sh)).pow(2))
                .sum::<i32>()
        })
        .map(|&(_, c, _)| c)
}

/// Flatten a component to text with its colours and formatting kept as
/// legacy `§` codes, which the HUD font draws.
pub fn legacy(tag: &Tag, lang: &Language) -> String {
    let mut out = String::new();
    append_legacy(tag, lang, &mut out, 0, &Legacy::default());
    out
}
fn append_legacy(tag: &Tag, lang: &Language, out: &mut String, depth: usize, parent: &Legacy) {
    if depth > 32 {
        return;
    }
    match tag {
        Tag::List(items) => {
            for item in items {
                append_legacy(item, lang, out, depth + 1, parent);
            }
        }
        Tag::Compound(_) => {
            let style = parent.over(tag);
            let styled = style != *parent;
            if styled {
                out.push_str(&style.codes());
            }
            if let Some(text) = get(tag, "text").and_then(string) {
                out.push_str(&text);
            } else if let Some(value) = get(tag, "") {
                append_legacy(value, lang, out, depth + 1, &style);
            } else if let Some(key) = get(tag, "translate").and_then(string) {
                let args: Vec<String> = match get(tag, "with") {
                    Some(Tag::List(args)) => args
                        .iter()
                        .map(|a| {
                            let mut s = String::new();
                            append_legacy(a, lang, &mut s, depth + 1, &style);
                            if s.contains('§') {
                                s.push_str(&style.codes());
                            }
                            s
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let pattern = lang
                    .get(&key)
                    .cloned()
                    .or_else(|| get(tag, "fallback").and_then(string))
                    .unwrap_or_else(|| key.clone());
                out.push_str(&format_java(&pattern, &args));
            } else {
                // Keybinds, selectors and scores have no styled parts.
                let mut bare = String::new();
                append(&strip_extra(tag), lang, &mut bare, depth + 1);
                out.push_str(&bare);
            }
            if let Some(Tag::List(extra)) = get(tag, "extra") {
                for e in extra {
                    let before = out.len();
                    append_legacy(e, lang, out, depth + 1, &style);
                    if out[before..].contains('§') {
                        out.push_str(&style.codes());
                    }
                }
            }
            if styled {
                out.push_str(&parent.codes());
            }
        }
        other => append(other, lang, out, depth + 1),
    }
}
fn strip_extra(tag: &Tag) -> Tag {
    match tag {
        Tag::Compound(map) => {
            let mut map = map.clone();
            map.remove(&JavaString::from("extra"));
            Tag::Compound(map)
        }
        other => other.clone(),
    }
}

/// The text a component shows on hover (`hover_event` show_text), the
/// first one found, in legacy `§` codes.
pub fn hover(tag: &Tag, lang: &Language) -> Option<String> {
    fn find<'t>(tag: &'t Tag, depth: usize) -> Option<&'t Tag> {
        if depth > 32 {
            return None;
        }
        match tag {
            Tag::List(items) => items.iter().find_map(|t| find(t, depth + 1)),
            Tag::Compound(_) => {
                for key in ["hover_event", "hoverEvent"] {
                    if let Some(h) = get(tag, key)
                        && get(h, "action").and_then(string).as_deref() == Some("show_text")
                        && let Some(value) = get(h, "value").or_else(|| get(h, "contents"))
                    {
                        return Some(value);
                    }
                }
                [get(tag, "with"), get(tag, "extra"), get(tag, "")]
                    .into_iter()
                    .flatten()
                    .find_map(|t| find(t, depth + 1))
            }
            _ => None,
        }
    }
    find(tag, 0).map(|value| legacy(value, lang))
}

/// The translation key of a component's root (`translate`), if it has one.
pub fn translate_key(tag: &Tag) -> Option<String> {
    get(tag, "translate").and_then(string)
}

/// TranslatableContents.decomposeTemplate: `%s`, `%n$s` and `%%`.
pub fn format_java(pattern: &str, args: &[String]) -> String {
    let mut out = String::new();
    let mut next = 0usize;
    let mut chars = pattern.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut digits = String::new();
        while let Some(&(_, d)) = chars.peek() {
            if d.is_ascii_digit() {
                digits.push(d);
                chars.next();
            } else {
                break;
            }
        }
        match chars.next() {
            Some((_, '%')) if digits.is_empty() => out.push('%'),
            Some((_, '$')) if !digits.is_empty() => {
                if chars.next().map(|(_, c)| c) == Some('s') {
                    let i = digits.parse::<usize>().unwrap_or(1).saturating_sub(1);
                    out.push_str(args.get(i).map_or("", String::as_str));
                }
            }
            Some((_, 's')) if digits.is_empty() => {
                out.push_str(args.get(next).map_or("", String::as_str));
                next += 1;
            }
            Some((_, other)) => {
                out.push('%');
                out.push_str(&digits);
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// A literal text component.
pub fn literal(text: &str) -> Tag {
    Tag::String(JavaString::from(text))
}
/// A translatable component with literal arguments.
pub fn translatable(key: &str, args: &[&str]) -> Tag {
    let mut entries = vec![("translate", Tag::String(JavaString::from(key)))];
    if !args.is_empty() {
        entries.push(("with", Tag::List(args.iter().map(|a| literal(a)).collect())));
    }
    Tag::compound(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_colours_and_hover_as_legacy_codes() {
        let lang: Language = [(
            "chat.type.advancement.task".to_owned(),
            "%s has made the advancement %s".to_owned(),
        )]
        .into();
        let name = Tag::compound([
            ("text", literal("[Stone Age]")),
            ("color", literal("green")),
            (
                "hover_event",
                Tag::compound([
                    ("action", literal("show_text")),
                    (
                        "value",
                        Tag::compound([("text", literal("Mine Stone")), ("color", literal("#55FF50"))]),
                    ),
                ]),
            ),
        ]);
        let tag = Tag::compound([
            ("translate", literal("chat.type.advancement.task")),
            ("with", Tag::List(vec![literal("Ann"), name])),
        ]);
        assert_eq!(
            legacy(&tag, &lang),
            "Ann has made the advancement §r§a[Stone Age]§r§r"
        );
        assert_eq!(plain(&tag, &lang), "Ann has made the advancement [Stone Age]");
        assert_eq!(hover(&tag, &lang).as_deref(), Some("§r§aMine Stone§r"));
        assert_eq!(translate_key(&tag).as_deref(), Some("chat.type.advancement.task"));
    }

    #[test]
    fn formats_like_translatable_contents() {
        let args = vec!["Steve".to_owned(), "hi".to_owned()];
        assert_eq!(format_java("<%s> %s", &args), "<Steve> hi");
        assert_eq!(format_java("%2$s by %1$s", &args), "hi by Steve");
        assert_eq!(format_java("100%% sure", &[]), "100% sure");
    }

    #[test]
    fn unwraps_mixed_list_arguments() {
        let mut lang = Language::new();
        lang.insert(
            "commands.item.entity.set.success.single".into(),
            "Replaced %s slot(s) on %s with %s".into(),
        );
        // `with: [1, {text: "Alex"}, ...]` in NBT: the int comes wrapped.
        let replaced = Tag::compound([
            (
                "translate",
                Tag::String("commands.item.entity.set.success.single".into()),
            ),
            (
                "with",
                Tag::List(vec![
                    Tag::compound([("", Tag::Int(1))]),
                    Tag::compound([("text", Tag::String("Alex".into()))]),
                    Tag::compound([("", Tag::String("[Emerald]".into()))]),
                ]),
            ),
        ]);
        assert_eq!(
            plain(&replaced, &lang),
            "Replaced 1 slot(s) on Alex with [Emerald]"
        );
    }

    #[test]
    fn flattens_components() {
        let mut lang = Language::new();
        lang.insert(
            "multiplayer.player.joined".into(),
            "%s joined the game".into(),
        );
        let joined = Tag::compound([
            ("translate", Tag::String("multiplayer.player.joined".into())),
            (
                "with",
                Tag::List(vec![Tag::compound([("text", Tag::String("Alex".into()))])]),
            ),
            ("color", Tag::String("yellow".into())),
        ]);
        assert_eq!(plain(&joined, &lang), "Alex joined the game");
        let extra = Tag::compound([
            ("text", Tag::String("a".into())),
            (
                "extra",
                Tag::List(vec![
                    literal("b"),
                    Tag::compound([("text", Tag::String("c".into()))]),
                ]),
            ),
        ]);
        assert_eq!(plain(&extra, &lang), "abc");
        assert_eq!(
            plain(&translatable("missing.key", &[]), &lang),
            "missing.key"
        );
        assert_eq!(plain(&literal("plain"), &lang), "plain");
    }
}
