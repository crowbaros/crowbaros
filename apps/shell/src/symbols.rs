//! A deliberately small, local catalogue. Names and keywords are our own.
#[derive(Clone, Copy)]
pub struct Symbol {
    pub glyph: &'static str,
    pub name: &'static str,
    pub category: &'static str,
    keywords: &'static str,
}
macro_rules! symbols {
    ($($glyph:literal, $name:literal, $category:literal, $keywords:literal;)*) => {
        pub const SYMBOLS: &[Symbol] = &[$(Symbol { glyph: $glyph, name: $name, category: $category, keywords: $keywords }),*];
    };
}
symbols! {
    "—", "Em dash", "Typography", "punctuation long dash";
    "–", "En dash", "Typography", "punctuation range short dash";
    "·", "Middle dot", "Typography", "bullet separator multiply";
    "…", "Ellipsis", "Typography", "dots punctuation";
    "“", "Left double quote", "Typography", "opening quotation curly";
    "”", "Right double quote", "Typography", "closing quotation curly";
    "‘", "Left single quote", "Typography", "opening quotation curly";
    "’", "Right single quote", "Typography", "apostrophe closing quotation curly";
    "•", "Bullet", "Typography", "list dot";
    "©", "Copyright", "Typography", "legal rights";
    "®", "Registered trademark", "Typography", "legal rights";
    "™", "Trademark", "Typography", "legal rights";
    "§", "Section sign", "Typography", "paragraph legal";
    "¶", "Paragraph sign", "Typography", "pilcrow";
    "→", "Right arrow", "Arrows", "next forward direction";
    "←", "Left arrow", "Arrows", "back previous direction";
    "↑", "Up arrow", "Arrows", "above direction";
    "↓", "Down arrow", "Arrows", "below direction";
    "↔", "Left right arrow", "Arrows", "both horizontal direction";
    "⇒", "Right double arrow", "Arrows", "implies therefore";
    "⌘", "Command", "Symbols", "keyboard mac shortcut";
    "⌥", "Option", "Symbols", "keyboard alt mac shortcut";
    "✓", "Check mark", "Symbols", "yes done tick success";
    "✗", "Cross mark", "Symbols", "no wrong error";
    "★", "Star", "Symbols", "favorite rating";
    "×", "Multiplication", "Math", "multiply times product";
    "÷", "Division", "Math", "divide quotient";
    "−", "Minus", "Math", "subtract negative";
    "±", "Plus minus", "Math", "positive negative tolerance";
    "≠", "Not equal", "Math", "inequality differs";
    "≈", "Approximately equal", "Math", "about estimate";
    "≤", "Less than or equal", "Math", "inequality limit";
    "≥", "Greater than or equal", "Math", "inequality limit";
    "∞", "Infinity", "Math", "unbounded forever";
    "√", "Square root", "Math", "radical sqrt";
    "π", "Pi", "Math", "circle greek";
    "∑", "Summation", "Math", "sum sigma total";
    "∫", "Integral", "Math", "calculus";
    "°", "Degree", "Math", "temperature angle";
    "²", "Superscript two", "Math", "squared power exponent";
    "³", "Superscript three", "Math", "cubed power exponent";
    "½", "One half", "Math", "fraction";
    "¼", "One quarter", "Math", "fraction";
    "¾", "Three quarters", "Math", "fraction";
    "€", "Euro", "Currency", "money europe";
    "£", "Pound sterling", "Currency", "money british gbp";
    "¥", "Yen", "Currency", "money japan yuan china";
    "¢", "Cent", "Currency", "money cents";
    "₹", "Indian rupee", "Currency", "money india inr";
    "₩", "Won", "Currency", "money korea";
    "₿", "Bitcoin", "Currency", "money btc";
    "😀", "Grinning face", "Emoji", "happy smile hello";
    "😊", "Smiling face", "Emoji", "happy blush pleased";
    "😂", "Face with tears of joy", "Emoji", "laugh funny lol";
    "😅", "Grinning face with sweat", "Emoji", "relief nervous";
    "😍", "Smiling face with heart eyes", "Emoji", "love adore";
    "🤔", "Thinking face", "Emoji", "hmm question consider";
    "😢", "Crying face", "Emoji", "sad tear";
    "😎", "Face with sunglasses", "Emoji", "cool sunny";
    "👍", "Thumbs up", "Emoji", "yes approve good like";
    "👎", "Thumbs down", "Emoji", "no disapprove dislike";
    "👏", "Clapping hands", "Emoji", "applause bravo well done";
    "🙌", "Raising hands", "Emoji", "celebrate hooray";
    "👋", "Waving hand", "Emoji", "hello goodbye wave";
    "🙏", "Folded hands", "Emoji", "thanks please gratitude";
    "❤️", "Red heart", "Emoji", "love affection";
    "🎉", "Party popper", "Emoji", "celebration congratulations confetti";
    "🔥", "Fire", "Emoji", "hot flame";
    "🚀", "Rocket", "Emoji", "launch space ship";
    "💡", "Light bulb", "Emoji", "idea insight";
    "✅", "Check button", "Emoji", "yes done tick success";
    "⚠️", "Warning", "Emoji", "caution alert";
    "☀️", "Sun", "Emoji", "sunny weather day";
    "🌙", "Crescent moon", "Emoji", "night sleep";
    "☕", "Coffee", "Emoji", "hot beverage tea break";
    "🌱", "Seedling", "Emoji", "plant growth nature";
}

pub fn search(query: &str) -> Vec<&'static Symbol> {
    let query = query.trim().to_lowercase();
    let words: Vec<_> = query.split_whitespace().collect();
    let mut matches: Vec<_> = SYMBOLS
        .iter()
        .filter_map(|s| {
            let name = s.name.to_lowercase();
            let haystack = format!("{name} {} {} {}", s.category.to_lowercase(), s.keywords, s.glyph);
            if !words.iter().all(|word| haystack.contains(word)) {
                return None;
            }
            let rank = if query.is_empty() || name == query || s.glyph == query {
                0
            } else if name.starts_with(&query) {
                1
            } else if name.split_whitespace().any(|word| word == query) {
                2
            } else if name.contains(&query) {
                3
            } else {
                4
            };
            Some((rank, s))
        })
        .collect();
    matches.sort_by_key(|(rank, _)| *rank);
    matches.into_iter().map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_rank_before_keywords_and_queries_ignore_case() {
        assert_eq!(search("  em DASH ")[0].glyph, "—");
        assert_eq!(search("multiply")[0].glyph, "·");
        assert_eq!(search("Multiplication")[0].glyph, "×");
        assert_eq!(search("right arrow")[0].glyph, "→");
        assert_eq!(search("♥ missing").len(), 0);
        assert_eq!(search("rocket")[0].glyph, "🚀");
        assert_eq!(search("thanks")[0].glyph, "🙏");
        assert_eq!(search("€")[0].name, "Euro");
        assert_eq!(search("emoji happy").len(), 2);
        assert_eq!(search("").len(), SYMBOLS.len());
    }
}
