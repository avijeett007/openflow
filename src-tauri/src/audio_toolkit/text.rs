use crate::settings::DictionaryEntry;
use natural::phonetics::soundex;
use once_cell::sync::Lazy;
use regex::Regex;
use strsim::levenshtein;

/// Builds an n-gram string by cleaning and concatenating words
///
/// Strips punctuation from each word, lowercases, and joins without spaces.
/// This allows matching "Charge B" against "ChargeBee".
fn build_ngram(words: &[&str]) -> String {
    words
        .iter()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .collect::<Vec<_>>()
        .concat()
}

/// A precomputed exact-match alias: the alias split into normalized tokens plus
/// the canonical replacement and casing policy.
struct AliasMatcher {
    /// Alias tokens, normalized (punctuation-trimmed; lowercased unless the entry
    /// is case-sensitive). Length drives greedy longest-match n-gram selection.
    tokens: Vec<String>,
    canonical: String,
    case_sensitive: bool,
}

/// A precomputed fuzzy target (a canonical word or one of its aliases) compared
/// against candidate n-grams by Levenshtein distance + Soundex.
struct FuzzyTarget {
    /// Lowercased, space-stripped comparison form (matches `build_ngram` output).
    compare: String,
    /// Canonical replacement emitted on a match.
    canonical: String,
    case_sensitive: bool,
}

/// Normalizes a single word for exact alias comparison: strips leading/trailing
/// punctuation, and lowercases unless the entry is case-sensitive.
fn normalize_token(word: &str, case_sensitive: bool) -> String {
    let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric());
    if case_sensitive {
        trimmed.to_string()
    } else {
        trimmed.to_lowercase()
    }
}

/// Builds the exact-alias matchers for all entries. Aliases with no usable tokens
/// (empty / all-punctuation) are dropped.
fn build_alias_matchers(entries: &[DictionaryEntry]) -> Vec<AliasMatcher> {
    let mut matchers = Vec::new();
    for entry in entries {
        for alias in &entry.sounds_like {
            let tokens: Vec<String> = alias
                .split_whitespace()
                .map(|w| normalize_token(w, entry.case_sensitive))
                .filter(|t| !t.is_empty())
                .collect();
            if tokens.is_empty() {
                continue;
            }
            matchers.push(AliasMatcher {
                tokens,
                canonical: entry.word.clone(),
                case_sensitive: entry.case_sensitive,
            });
        }
    }
    matchers
}

/// Builds the fuzzy targets: every canonical word and every alias, each resolving
/// to the entry's canonical word. Entries flagged `replace_exact` are excluded —
/// they participate only in the deterministic alias pass.
fn build_fuzzy_targets(entries: &[DictionaryEntry]) -> Vec<FuzzyTarget> {
    let mut targets = Vec::new();
    for entry in entries {
        if entry.replace_exact {
            continue;
        }
        let canonical_cmp = entry.word.to_lowercase().replace(' ', "");
        if !canonical_cmp.is_empty() {
            targets.push(FuzzyTarget {
                compare: canonical_cmp,
                canonical: entry.word.clone(),
                case_sensitive: entry.case_sensitive,
            });
        }
        for alias in &entry.sounds_like {
            let alias_cmp = alias.to_lowercase().replace(' ', "");
            if !alias_cmp.is_empty() {
                targets.push(FuzzyTarget {
                    compare: alias_cmp,
                    canonical: entry.word.clone(),
                    case_sensitive: entry.case_sensitive,
                });
            }
        }
    }
    targets
}

/// Tries to match an alias exactly at the start of `words`, preferring the alias
/// with the most tokens (greedy). Returns the matched token count and matcher.
fn match_exact_alias<'a>(
    words: &[&str],
    matchers: &'a [AliasMatcher],
) -> Option<(usize, &'a AliasMatcher)> {
    let mut best: Option<(usize, &AliasMatcher)> = None;
    for matcher in matchers {
        let n = matcher.tokens.len();
        if n > words.len() {
            continue;
        }
        let all_match = matcher
            .tokens
            .iter()
            .enumerate()
            .all(|(k, tok)| normalize_token(words[k], matcher.case_sensitive) == *tok);
        if !all_match {
            continue;
        }
        match best {
            Some((bn, _)) if bn >= n => {}
            _ => best = Some((n, matcher)),
        }
    }
    best
}

/// Finds the best fuzzy target for a candidate n-gram string.
///
/// Uses Levenshtein distance and Soundex phonetic matching, with a 25% length
/// guard to prevent an n-gram from matching a much shorter target.
fn find_best_fuzzy<'a>(
    candidate: &str,
    targets: &'a [FuzzyTarget],
    threshold: f64,
) -> Option<&'a FuzzyTarget> {
    if candidate.is_empty() || candidate.len() > 50 {
        return None;
    }

    let mut best_target: Option<&FuzzyTarget> = None;
    let mut best_score = f64::MAX;

    for target in targets {
        let compare = &target.compare;
        // Skip if lengths are too different (optimization + prevents over-matching)
        // Use percentage-based check: max 25% length difference (prevents n-grams from
        // matching significantly shorter targets, e.g., "openaigpt" vs "openai")
        let len_diff = (candidate.len() as i32 - compare.len() as i32).abs() as f64;
        let max_len = candidate.len().max(compare.len()) as f64;
        let max_allowed_diff = (max_len * 0.25).max(2.0); // At least 2 chars difference allowed
        if len_diff > max_allowed_diff {
            continue;
        }

        // Calculate Levenshtein distance (normalized by length)
        let levenshtein_dist = levenshtein(candidate, compare);
        let levenshtein_score = if max_len > 0.0 {
            levenshtein_dist as f64 / max_len
        } else {
            1.0
        };

        // Calculate phonetic similarity using Soundex
        let phonetic_match = soundex(candidate, compare);

        // Combine scores: favor phonetic matches, but also consider string similarity
        let combined_score = if phonetic_match {
            levenshtein_score * 0.3 // Give significant boost to phonetic matches
        } else {
            levenshtein_score
        };

        // Accept if the score is good enough (configurable threshold)
        if combined_score < threshold && combined_score < best_score {
            best_target = Some(target);
            best_score = combined_score;
        }
    }

    best_target
}

/// Applies dictionary corrections to transcribed text.
///
/// Two passes run left-to-right, greedy longest-match first:
/// 1. **Deterministic alias replacement** — any n-gram exactly matching an
///    entry's `sounds_like` alias is rewritten to the canonical `word`. This is
///    threshold-independent, so homophone rules always fire.
/// 2. **Fuzzy correction** — Levenshtein + Soundex n-gram matching against
///    canonical words *and* aliases (a fuzzy alias hit yields the canonical word).
///    Entries flagged `replace_exact` skip this pass entirely.
///
/// `case_sensitive` entries emit `word` verbatim; otherwise the input token's
/// case pattern is preserved. Punctuation around a match is preserved.
///
/// # Arguments
/// * `text` - The input text to correct
/// * `entries` - Dictionary entries to match against
/// * `threshold` - Maximum fuzzy similarity score to accept (0.0 = exact only)
pub fn apply_dictionary(text: &str, entries: &[DictionaryEntry], threshold: f64) -> String {
    apply_dictionary_inner(text, entries, threshold, true)
}

/// Deterministic alias replacement only, with the fuzzy pass disabled.
///
/// Used on the whisper-prompted path: the canonical words were already handed to
/// the model as an initial prompt (so fuzzy correction is redundant and skipped),
/// but explicit `sounds_like` alias rules must still be enforced.
pub fn apply_dictionary_aliases_only(text: &str, entries: &[DictionaryEntry]) -> String {
    apply_dictionary_inner(text, entries, 0.0, false)
}

/// A token after the exact-alias pass. `Locked` tokens are the result of a
/// deterministic alias replacement and are immune to the fuzzy pass — which also
/// may not span them, so a fuzzy n-gram can't swallow a word adjacent to an
/// already-resolved alias.
enum Token<'a> {
    Raw(&'a str),
    Locked(String),
}

fn apply_dictionary_inner(
    text: &str,
    entries: &[DictionaryEntry],
    threshold: f64,
    fuzzy_enabled: bool,
) -> String {
    if entries.is_empty() {
        return text.to_string();
    }

    let alias_matchers = build_alias_matchers(entries);
    let words: Vec<&str> = text.split_whitespace().collect();

    // Pass 1: deterministic exact alias replacement (threshold-independent),
    // greedy longest-match. Replaced spans are locked so the fuzzy pass leaves
    // them (and their neighbors) alone.
    let mut stage1: Vec<Token> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if let Some((n, matcher)) = match_exact_alias(&words[i..], &alias_matchers) {
            let ngram_words = &words[i..i + n];
            let (prefix, _) = extract_punctuation(ngram_words[0]);
            let (_, suffix) = extract_punctuation(ngram_words[n - 1]);
            let corrected = if matcher.case_sensitive {
                matcher.canonical.clone()
            } else {
                preserve_case_pattern(ngram_words[0], &matcher.canonical)
            };
            stage1.push(Token::Locked(format!("{}{}{}", prefix, corrected, suffix)));
            i += n;
        } else {
            stage1.push(Token::Raw(words[i]));
            i += 1;
        }
    }

    let render = |tokens: &[Token]| -> String {
        tokens
            .iter()
            .map(|t| match t {
                Token::Raw(s) => (*s).to_string(),
                Token::Locked(s) => s.clone(),
            })
            .collect::<Vec<_>>()
            .join(" ")
    };

    if !fuzzy_enabled {
        return render(&stage1);
    }

    let fuzzy_targets = build_fuzzy_targets(entries);

    // Pass 2: fuzzy correction over canonical words + aliases (greedy n-grams),
    // skipping locked tokens entirely.
    let mut result: Vec<String> = Vec::new();
    let mut i = 0;
    while i < stage1.len() {
        if let Token::Locked(s) = &stage1[i] {
            result.push(s.clone());
            i += 1;
            continue;
        }

        let mut matched = false;
        for n in (1..=3).rev() {
            if i + n > stage1.len() {
                continue;
            }
            let span = &stage1[i..i + n];
            // An n-gram may not span a locked (already-resolved) token.
            if span.iter().any(|t| matches!(t, Token::Locked(_))) {
                continue;
            }
            let ngram_words: Vec<&str> = span
                .iter()
                .map(|t| match t {
                    Token::Raw(s) => *s,
                    Token::Locked(_) => unreachable!("locked tokens filtered above"),
                })
                .collect();
            let ngram = build_ngram(&ngram_words);

            if let Some(target) = find_best_fuzzy(&ngram, &fuzzy_targets, threshold) {
                let (prefix, _) = extract_punctuation(ngram_words[0]);
                let (_, suffix) = extract_punctuation(ngram_words[n - 1]);
                let corrected = if target.case_sensitive {
                    target.canonical.clone()
                } else {
                    preserve_case_pattern(ngram_words[0], &target.canonical)
                };
                result.push(format!("{}{}{}", prefix, corrected, suffix));
                i += n;
                matched = true;
                break;
            }
        }

        if !matched {
            if let Token::Raw(s) = &stage1[i] {
                result.push((*s).to_string());
            }
            i += 1;
        }
    }

    result.join(" ")
}

/// Legacy fuzzy-only entry point for callers that still pass a flat word list.
/// Each word becomes a dictionary entry with no aliases.
pub fn apply_custom_words(text: &str, custom_words: &[String], threshold: f64) -> String {
    let entries: Vec<DictionaryEntry> = custom_words
        .iter()
        .map(|word| DictionaryEntry {
            word: word.clone(),
            sounds_like: Vec::new(),
            replace_exact: false,
            case_sensitive: false,
        })
        .collect();
    apply_dictionary(text, &entries, threshold)
}

/// Preserves the case pattern of the original word when applying a replacement
fn preserve_case_pattern(original: &str, replacement: &str) -> String {
    if original.chars().all(|c| c.is_uppercase()) {
        replacement.to_uppercase()
    } else if original.chars().next().is_some_and(|c| c.is_uppercase()) {
        let mut chars: Vec<char> = replacement.chars().collect();
        if let Some(first_char) = chars.get_mut(0) {
            *first_char = first_char.to_uppercase().next().unwrap_or(*first_char);
        }
        chars.into_iter().collect()
    } else {
        replacement.to_string()
    }
}

/// Extracts punctuation prefix and suffix from a word
fn extract_punctuation(word: &str) -> (&str, &str) {
    let prefix_end = word.chars().take_while(|c| !c.is_alphanumeric()).count();
    let suffix_start = word
        .char_indices()
        .rev()
        .take_while(|(_, c)| !c.is_alphanumeric())
        .count();

    let prefix = if prefix_end > 0 {
        &word[..prefix_end]
    } else {
        ""
    };

    let suffix = if suffix_start > 0 {
        &word[word.len() - suffix_start..]
    } else {
        ""
    };

    (prefix, suffix)
}

/// Returns filler words appropriate for the given language code.
///
/// Some words like "um" and "ha" are real words in certain languages
/// (e.g., Portuguese "um" = "a/an", Spanish "ha" = "has"), so we only
/// include them as fillers for languages where they are truly fillers.
fn get_filler_words_for_language(lang: &str) -> &'static [&'static str] {
    let base_lang = lang.split(&['-', '_'][..]).next().unwrap_or(lang);

    match base_lang {
        "en" => &[
            "uh", "um", "uhm", "umm", "uhh", "uhhh", "ah", "hmm", "hm", "mmm", "mm", "mh", "eh",
            "ehh", "ha",
        ],
        "es" => &["ehm", "mmm", "hmm", "hm"],
        "pt" => &["ahm", "hmm", "mmm", "hm"],
        "fr" => &["euh", "hmm", "hm", "mmm"],
        "de" => &["äh", "ähm", "hmm", "hm", "mmm"],
        "it" => &["ehm", "hmm", "mmm", "hm"],
        "cs" => &["ehm", "hmm", "mmm", "hm"],
        "pl" => &["hmm", "mmm", "hm"],
        "tr" => &["hmm", "mmm", "hm"],
        "ru" => &["хм", "ммм", "hmm", "mmm"],
        "uk" => &["хм", "ммм", "hmm", "mmm"],
        "ar" => &["hmm", "mmm"],
        "ja" => &["hmm", "mmm"],
        "ko" => &["hmm", "mmm"],
        "vi" => &["hmm", "mmm", "hm"],
        "zh" => &["hmm", "mmm"],
        // Conservative universal fallback (no "um", "eh", "ha")
        _ => &[
            "uh", "uhm", "umm", "uhh", "uhhh", "ah", "hmm", "hm", "mmm", "mm", "mh", "ehh",
        ],
    }
}

static MULTI_SPACE_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s{2,}").unwrap());

/// Collapses repeated words (3+ repetitions) to a single instance.
/// E.g., "wh wh wh wh" -> "wh", "I I I I" -> "I"
fn collapse_stutters(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return text.to_string();
    }

    let mut result: Vec<&str> = Vec::new();
    let mut i = 0;

    while i < words.len() {
        let word = words[i];
        let word_lower = word.to_lowercase();

        if word_lower.chars().all(|c| c.is_alphabetic()) {
            // Count consecutive repetitions (case-insensitive)
            let mut count = 1;
            while i + count < words.len() && words[i + count].to_lowercase() == word_lower {
                count += 1;
            }

            // If 3+ repetitions, collapse to single instance
            if count >= 3 {
                result.push(word);
                i += count;
            } else {
                result.push(word);
                i += 1;
            }
        } else {
            result.push(word);
            i += 1;
        }
    }

    result.join(" ")
}

/// Filters transcription output by removing filler words and stutter artifacts.
///
/// This function cleans up raw transcription text by:
/// 1. Removing filler words based on the app language (or custom list)
/// 2. Collapsing repeated word stutters (e.g., "wh wh wh" -> "wh")
/// 3. Cleaning up excess whitespace
///
/// # Arguments
/// * `text` - The raw transcription text to filter
/// * `lang` - The app language code (e.g., "en", "pt-BR") used to select filler words
/// * `custom_filler_words` - Optional user-provided filler word list. `Some(vec)` overrides
///   language defaults; `Some(empty vec)` disables filtering; `None` uses language defaults.
///
/// # Returns
/// The filtered text with filler words and stutters removed
pub fn filter_transcription_output(
    text: &str,
    lang: &str,
    custom_filler_words: &Option<Vec<String>>,
) -> String {
    let mut filtered = text.to_string();

    // Build filler patterns from custom list or language defaults
    let patterns: Vec<Regex> = match custom_filler_words {
        Some(words) => words
            .iter()
            .filter_map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).ok())
            .collect(),
        None => get_filler_words_for_language(lang)
            .iter()
            .map(|word| Regex::new(&format!(r"(?i)\b{}\b[,.]?", regex::escape(word))).unwrap())
            .collect(),
    };

    // Remove filler words
    for pattern in &patterns {
        filtered = pattern.replace_all(&filtered, "").to_string();
    }

    // Collapse repeated 1-2 letter words (stutter artifacts like "wh wh wh wh")
    filtered = collapse_stutters(&filtered);

    // Clean up multiple spaces to single space
    filtered = MULTI_SPACE_PATTERN.replace_all(&filtered, " ").to_string();

    // Trim leading/trailing whitespace
    filtered.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_custom_words_exact_match() {
        let text = "hello world";
        let custom_words = vec!["Hello".to_string(), "World".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_apply_custom_words_fuzzy_match() {
        let text = "helo wrold";
        let custom_words = vec!["hello".to_string(), "world".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_preserve_case_pattern() {
        assert_eq!(preserve_case_pattern("HELLO", "world"), "WORLD");
        assert_eq!(preserve_case_pattern("Hello", "world"), "World");
        assert_eq!(preserve_case_pattern("hello", "WORLD"), "WORLD");
    }

    #[test]
    fn test_extract_punctuation() {
        assert_eq!(extract_punctuation("hello"), ("", ""));
        assert_eq!(extract_punctuation("!hello?"), ("!", "?"));
        assert_eq!(extract_punctuation("...hello..."), ("...", "..."));
    }

    #[test]
    fn test_empty_custom_words() {
        let text = "hello world";
        let custom_words = vec![];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_filter_filler_words() {
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "So I was thinking about this");
    }

    #[test]
    fn test_filter_filler_words_case_insensitive() {
        let text = "UHM this is UH a test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "this is a test");
    }

    #[test]
    fn test_filter_filler_words_with_punctuation() {
        let text = "Well, uhm, I think, uh. that's right";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Well, I think, that's right");
    }

    #[test]
    fn test_filter_cleans_whitespace() {
        let text = "Hello    world   test";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world test");
    }

    #[test]
    fn test_filter_trims() {
        let text = "  Hello world  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_filter_combined() {
        let text = "  Uhm, so I was, uh, thinking about this  ";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "so I was, thinking about this");
    }

    #[test]
    fn test_filter_preserves_valid_text() {
        let text = "This is a completely normal sentence.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "This is a completely normal sentence.");
    }

    #[test]
    fn test_filter_stutter_collapse() {
        let text = "w wh wh wh wh wh wh wh wh wh why";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "w wh why");
    }

    #[test]
    fn test_filter_stutter_short_words() {
        let text = "I I I I think so so so so";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think so");
    }

    #[test]
    fn test_filter_stutter_longer_words() {
        let text = "Check data doc doc doc doc documentation.";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "Check data doc documentation.");
    }

    #[test]
    fn test_filter_stutter_mixed_case() {
        let text = "No NO no NO no";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "No");
    }

    #[test]
    fn test_filter_stutter_preserves_two_repetitions() {
        let text = "no no is fine";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "no no is fine");
    }

    #[test]
    fn test_filter_english_removes_um() {
        let text = "um I think um this is good";
        let result = filter_transcription_output(text, "en", &None);
        assert_eq!(result, "I think this is good");
    }

    #[test]
    fn test_filter_portuguese_preserves_um() {
        // "um" means "a/an" in Portuguese
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_spanish_preserves_ha() {
        // "ha" means "has" in Spanish
        let text = "ha sido un buen día";
        let result = filter_transcription_output(text, "es", &None);
        assert_eq!(result, "ha sido un buen día");
    }

    #[test]
    fn test_filter_language_code_with_region() {
        // "pt-BR" should normalize to "pt"
        let text = "um gato bonito";
        let result = filter_transcription_output(text, "pt-BR", &None);
        assert_eq!(result, "um gato bonito");
    }

    #[test]
    fn test_filter_custom_filler_words_override() {
        let custom = Some(vec!["okay".to_string(), "right".to_string()]);
        let text = "okay so I think right this works";
        let result = filter_transcription_output(text, "en", &custom);
        assert_eq!(result, "so I think this works");
    }

    #[test]
    fn test_filter_custom_filler_words_empty_disables() {
        let custom = Some(vec![]);
        let text = "So uhm I was thinking uh about this";
        let result = filter_transcription_output(text, "en", &custom);
        // No filler words removed since custom list is empty
        assert_eq!(result, "So uhm I was thinking uh about this");
    }

    #[test]
    fn test_filter_unknown_language_uses_fallback() {
        let text = "uh I think uhm this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "I think this works");
    }

    #[test]
    fn test_filter_fallback_does_not_remove_um() {
        // Fallback (unknown language) should not remove "um" since it's a real word in some languages
        let text = "um I think this works";
        let result = filter_transcription_output(text, "xx", &None);
        assert_eq!(result, "um I think this works");
    }

    #[test]
    fn test_apply_custom_words_ngram_two_words() {
        let text = "il cui nome è Charge B, che permette";
        let custom_words = vec!["ChargeBee".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("ChargeBee,"));
        assert!(!result.contains("Charge B"));
    }

    #[test]
    fn test_apply_custom_words_ngram_three_words() {
        let text = "use Chat G P T for this";
        let custom_words = vec!["ChatGPT".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("ChatGPT"));
    }

    #[test]
    fn test_apply_custom_words_prefers_longer_ngram() {
        let text = "Open AI GPT model";
        let custom_words = vec!["OpenAI".to_string(), "GPT".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert_eq!(result, "OpenAI GPT model");
    }

    #[test]
    fn test_apply_custom_words_ngram_preserves_case() {
        let text = "CHARGE B is great";
        let custom_words = vec!["ChargeBee".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("CHARGEBEE"));
    }

    #[test]
    fn test_apply_custom_words_ngram_with_spaces_in_custom() {
        // Custom word with space should also match against split words
        let text = "using Mac Book Pro";
        let custom_words = vec!["MacBook Pro".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        assert!(result.contains("MacBook"));
    }

    #[test]
    fn test_apply_custom_words_trailing_number_not_doubled() {
        // Verify that trailing non-alpha chars (like numbers) aren't double-counted
        // between build_ngram stripping them and extract_punctuation capturing them
        let text = "use GPT4 for this";
        let custom_words = vec!["GPT-4".to_string()];
        let result = apply_custom_words(text, &custom_words, 0.5);
        // Should NOT produce "GPT-44" (double-counting the trailing 4)
        assert!(
            !result.contains("GPT-44"),
            "got double-counted result: {}",
            result
        );
    }

    // ---- Dictionary (entries + aliases) tests ----

    fn entry(word: &str, sounds_like: &[&str]) -> DictionaryEntry {
        DictionaryEntry {
            word: word.to_string(),
            sounds_like: sounds_like.iter().map(|s| s.to_string()).collect(),
            replace_exact: false,
            case_sensitive: false,
        }
    }

    #[test]
    fn test_dictionary_exact_alias_single_word() {
        let entries = vec![entry("Kubernetes", &["kubernetis", "coober netties"])];
        let result = apply_dictionary("we deploy on kubernetis today", &entries, 0.18);
        assert_eq!(result, "we deploy on Kubernetes today");
    }

    #[test]
    fn test_dictionary_exact_alias_multi_word() {
        let entries = vec![entry("MySQL", &["my sequel"])];
        let result = apply_dictionary("store it in my sequel please", &entries, 0.18);
        assert_eq!(result, "store it in MySQL please");
    }

    #[test]
    fn test_dictionary_exact_alias_is_threshold_independent() {
        // threshold 0.0 disables all fuzzy matching, yet the exact alias must fire.
        let entries = vec![entry("ChargeBee", &["charge bee"])];
        let result = apply_dictionary("the charge bee invoice", &entries, 0.0);
        assert_eq!(result, "the ChargeBee invoice");
    }

    #[test]
    fn test_dictionary_exact_alias_preserves_punctuation() {
        let entries = vec![entry("ChargeBee", &["charge bee"])];
        let result = apply_dictionary("use charge bee, please", &entries, 0.0);
        assert!(result.contains("ChargeBee,"), "got: {}", result);
    }

    #[test]
    fn test_dictionary_fuzzy_still_works_on_canonical() {
        let entries = vec![entry("hello", &[]), entry("world", &[])];
        let result = apply_dictionary("helo wrold", &entries, 0.5);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_dictionary_replace_exact_disables_fuzzy_but_alias_fires() {
        let entries = vec![DictionaryEntry {
            word: "ChargeBee".to_string(),
            sounds_like: vec!["charge bee".to_string()],
            replace_exact: true,
            case_sensitive: false,
        }];

        // Fuzzy on the canonical word is disabled: a near-miss stays untouched.
        let fuzzy = apply_dictionary("the chargebe invoice", &entries, 0.5);
        assert_eq!(fuzzy, "the chargebe invoice");

        // But the deterministic alias still fires.
        let exact = apply_dictionary("the charge bee invoice", &entries, 0.5);
        assert_eq!(exact, "the ChargeBee invoice");
    }

    #[test]
    fn test_dictionary_case_sensitive_emits_verbatim() {
        let entries = vec![DictionaryEntry {
            word: "iOS".to_string(),
            sounds_like: vec!["i o s".to_string()],
            replace_exact: false,
            case_sensitive: true,
        }];

        // Sentence-start capitalization must NOT be mirrored onto the canonical.
        let result = apply_dictionary("I o s is great", &entries, 0.18);
        assert!(result.contains("iOS"), "got: {}", result);
        assert!(!result.contains("IOS"), "got: {}", result);
    }

    #[test]
    fn test_dictionary_case_insensitive_preserves_pattern() {
        // Without case_sensitive, an all-caps input token yields an all-caps output.
        let entries = vec![entry("ChargeBee", &["charge bee"])];
        let result = apply_dictionary("CHARGE BEE is great", &entries, 0.18);
        assert!(result.contains("CHARGEBEE"), "got: {}", result);
    }

    #[test]
    fn test_dictionary_fuzzy_alias_yields_canonical() {
        // A fuzzy hit on an alias resolves to the canonical word, not the alias.
        let entries = vec![entry("Kubernetes", &["kubernetes cluster"])];
        // Slight misspelling of the alias token still routes to canonical.
        let result = apply_dictionary("our kubernetis stack", &entries, 0.3);
        assert!(result.contains("Kubernetes"), "got: {}", result);
    }

    #[test]
    fn test_dictionary_aliases_only_skips_fuzzy() {
        let entries = vec![entry("ChargeBee", &["charge bee"])];

        // Exact alias fires...
        let aliased = apply_dictionary_aliases_only("the charge bee invoice", &entries);
        assert_eq!(aliased, "the ChargeBee invoice");

        // ...but fuzzy on the canonical word does not run in aliases-only mode.
        let fuzzy = apply_dictionary_aliases_only("the chargebe invoice", &entries);
        assert_eq!(fuzzy, "the chargebe invoice");
    }

    #[test]
    fn test_dictionary_empty_entries_noop() {
        let result = apply_dictionary("hello world", &[], 0.5);
        assert_eq!(result, "hello world");
    }
}
