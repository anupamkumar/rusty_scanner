use std::collections::HashSet;

/// NLP Part-of-Speech (POS) weighting configuration (`nlp:` section in `scanner_config.yaml`).
#[derive(Debug, Clone)]
pub struct NlpConfig {
    /// Master switch for POS-based noun/proper-noun weighting.
    pub enabled: bool,
    /// Weight assigned to Proper Nouns (creator/performer/studio names, e.g. `Abbie`, `Kendra`).
    pub proper_noun_weight: f64,
    /// Weight assigned to concrete & morphological Common Nouns (heavier than verbs/adjectives).
    pub noun_weight: f64,
    /// Weight assigned to Verbs and `-ing`/`-ed` gerunds/participles (`smoking`, `laying`).
    pub verb_weight: f64,
    /// Weight assigned to descriptive Adjectives (`candid`, `new`, `sexy`, `sweet`, `cruel`).
    pub adjective_weight: f64,
    /// Weight assigned to role/title Honorifics (`mistress`, `goddess`, `princess`, `lady`, `miss`).
    pub honorific_weight: f64,
    /// Weight assigned to closed-class Function Words (prepositions, determiners, conjunctions, adverbs).
    pub function_word_weight: f64,
    /// How strongly POS salience scales non-directory match scores (`0.0` = off, `0.35` = default).
    pub nlp_influence: f64,
    /// Minimum final confidence score after NLP POS weighting required to accept a match.
    pub min_effective_score: f64,
}

impl Default for NlpConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            proper_noun_weight: 1.00,
            noun_weight: 0.85,
            verb_weight: 0.50,
            adjective_weight: 0.45,
            honorific_weight: 0.35,
            function_word_weight: 0.15,
            nlp_influence: 0.35,
            min_effective_score: 0.84,
        }
    }
}

/// Coarse Part-of-Speech category for video title & anchor name tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PosTag {
    ProperNoun,
    Noun,
    Honorific,
    Verb,
    Adjective,
    FunctionWord,
}

impl PosTag {
    pub fn weight(self, cfg: &NlpConfig) -> f64 {
        match self {
            PosTag::ProperNoun => cfg.proper_noun_weight,
            PosTag::Noun => cfg.noun_weight,
            PosTag::Honorific => cfg.honorific_weight,
            PosTag::Verb => cfg.verb_weight,
            PosTag::Adjective => cfg.adjective_weight,
            PosTag::FunctionWord => cfg.function_word_weight,
        }
    }
}

const HONORIFIC_WORDS: &[&str] = &[
    "mistress",
    "goddess",
    "goddexx",
    "princess",
    "lady",
    "madam",
    "madame",
    "miss",
    "ms",
    "mrs",
    "queen",
    "empress",
    "domina",
    "dominatrix",
    "domme",
    "lord",
    "enchantress",
    "diva",
    "baroness",
    "countess",
];

const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "the", "in", "on", "at", "by", "for", "with", "about", "against", "between",
    "into", "through", "during", "before", "after", "above", "below", "to", "from", "up", "down",
    "under", "over", "again", "further", "then", "once", "here", "there", "when", "where", "why",
    "how", "all", "any", "both", "each", "few", "more", "most", "other", "some", "such", "no",
    "nor", "not", "only", "own", "same", "so", "than", "too", "very", "just", "now", "and", "but",
    "or", "yet", "if", "because", "as", "until", "while", "of", "off", "out", "i", "me", "my",
    "myself", "we", "our", "ours", "you", "your", "yours", "he", "him", "his", "she", "her",
    "hers", "it", "its", "they", "them", "their", "what", "which", "who", "whom", "this", "that",
    "these", "those", "am", "is", "are", "was", "were", "be", "been", "being", "have", "has",
    "had", "do", "does", "did", "will", "would", "shall", "should", "can", "could", "may",
    "might", "must", "n",
];

const ADJECTIVE_WORDS: &[&str] = &[
    "candid",
    "vintage",
    "new",
    "nwe",
    "old",
    "young",
    "hot",
    "sexy",
    "sweet",
    "mean",
    "cruel",
    "evil",
    "dirty",
    "rough",
    "hard",
    "soft",
    "long",
    "short",
    "big",
    "large",
    "small",
    "tiny",
    "little",
    "spoiled",
    "spoit",
    "bratty",
    "sadistic",
    "divine",
    "royal",
    "exquisite",
    "happy",
    "beautiful",
    "pretty",
    "cute",
    "gorgeous",
    "stunning",
    "kinky",
    "ruthless",
    "dangerous",
    "merciless",
    "brutal",
    "wicked",
    "devious",
    "furious",
    "violent",
    "dominant",
    "submissive",
    "useless",
    "poor",
    "good",
    "bad",
    "best",
    "real",
    "true",
    "pure",
    "full",
    "raw",
    "extreme",
    "ultimate",
    "special",
    "spicy",
    "strong",
    "deep",
    "high",
    "low",
    "bare",
    "naked",
    "nude",
    "wet",
    "sweaty",
    "smelly",
    "stinky",
    "filthy",
    "nasty",
    "messy",
    "french",
    "british",
    "english",
    "latin",
    "latina",
    "russian",
    "polish",
    "italian",
    "german",
    "american",
    "japanese",
    "chinese",
    "asian",
    "ebony",
    "blonde",
    "brunette",
    "redhead",
    "petite",
    "tall",
    " busty",
    "curvy",
    "skinny",
    "red",
    "blue",
    "black",
    "white",
    "pink",
    "gold",
    "golden",
    "silver",
    "solo",
    "pov",
    "closeup",
    "handsfree",
];

const VERB_WORDS: &[&str] = &[
    "smoke",
    "smokes",
    "smoking",
    "lay",
    "laying",
    "lie",
    "lying",
    "sit",
    "sitting",
    "stand",
    "standing",
    "walk",
    "walking",
    "trample",
    "trampling",
    "spit",
    "spitting",
    "lick",
    "licking",
    "suck",
    "sucking",
    "kiss",
    "kissing",
    "worship",
    "worshipping",
    "obey",
    "obeying",
    "crush",
    "crushing",
    "fight",
    "fighting",
    "kick",
    "kicking",
    "slap",
    "slapping",
    "whip",
    "whipping",
    "beat",
    "beating",
    "hurt",
    "hurting",
    "tease",
    "teasing",
    "edge",
    "edging",
    "edgeing",
    "clean",
    "cleaning",
    "talk",
    "talking",
    "play",
    "playing",
    "ride",
    "riding",
    "jerk",
    "jerking",
    "watch",
    "watching",
    "love",
    "loving",
    "make",
    "making",
    "take",
    "taking",
    "give",
    "giving",
    "beg",
    "begging",
    "crawl",
    "crawling",
    "gag",
    "gagging",
    "choke",
    "choking",
    "sneeze",
    "sneezing",
];

/// Generic common nouns frequently appearing in scene descriptions (concrete nouns, NOT personal proper names!).
const GENERIC_COMMON_NOUNS: &[&str] = &[
    "girl",
    "girls",
    "woman",
    "women",
    "boy",
    "boys",
    "man",
    "men",
    "slave",
    "slaves",
    "foot",
    "feet",
    "sole",
    "soles",
    "toe",
    "toes",
    "shoe",
    "shoes",
    "boot",
    "boots",
    "heel",
    "heels",
    "stiletto",
    "stilletto",
    "sock",
    "socks",
    "nylon",
    "nylons",
    "stocking",
    "stockings",
    "pantyhose",
    "jeans",
    "leather",
    "latex",
    "vinyl",
    "glasses",
    "couch",
    "bed",
    "bedroom",
    "office",
    "cafe",
    "club",
    "palace",
    "mansion",
    "empire",
    "house",
    "world",
    "planet",
    "network",
    "collection",
    "store",
    "video",
    "videos",
    "clip",
    "clips",
    "film",
    "films",
    "scene",
    "scenes",
    "episode",
    "part",
    "day",
    "night",
    "time",
    "life",
    "face",
    "ass",
    "leg",
    "legs",
    "hand",
    "hands",
    "mouth",
    "tongue",
    "sweat",
    "spit",
    "cum",
    "cig",
    "cigs",
    "cigarette",
    "cigarettes",
    "handjob",
    "handjobs",
    "footjob",
    "shoejob",
    "facesitting",
    "faceriding",
    "ballbusting",
    "femdom",
    "fetish",
    "fetishes",
    "bondage",
    "bdsm",
    "humiliation",
    "domination",
    "pain",
    "punishment",
    "punishments",
    "interview",
    "conversation",
    "profile",
    "close",
    "toilet",
    "ponies",
    "bitch",
    "bitches",
    "vixen",
    "vixens",
    "chicks",
    "beauties",
    "cuties",
    "bratz",
    "divas",
    "amazons",
    "queens",
    "dominas",
];

/// Compiled lexicon sets for $O(1)$ POS tagging across thousands of video titles.
#[derive(Debug, Clone)]
pub struct NlpAnalyzer {
    pub cfg: NlpConfig,
    honorifics: HashSet<&'static str>,
    function_words: HashSet<&'static str>,
    adjectives: HashSet<&'static str>,
    verbs: HashSet<&'static str>,
    common_nouns: HashSet<&'static str>,
    /// Known Proper Noun tokens mined from multi-word / personal-name anchors in `anchors.yaml`.
    known_proper_nouns: HashSet<String>,
}

impl NlpAnalyzer {
    pub fn new(cfg: NlpConfig, anchor_display_names: &[String]) -> Self {
        let honorifics: HashSet<&'static str> = HONORIFIC_WORDS.iter().copied().collect();
        let function_words: HashSet<&'static str> = FUNCTION_WORDS.iter().copied().collect();
        let adjectives: HashSet<&'static str> = ADJECTIVE_WORDS.iter().copied().collect();
        let verbs: HashSet<&'static str> = VERB_WORDS.iter().copied().collect();
        let common_nouns: HashSet<&'static str> = GENERIC_COMMON_NOUNS.iter().copied().collect();

        // Extract high-confidence proper-noun tokens from anchor names:
        // 1. Tokens following an honorific (`Mistress Kendra`, `Goddess Gwen`, `Princess Ellie`)
        // 2. Non-lexicon tokens in creator/studio names (`Abbie`, `Kendra`, `Dommelia`, `Weronika`)
        let mut known_proper_nouns: HashSet<String> = HashSet::new();
        for raw in anchor_display_names {
            let cleaned = raw
                .trim_end_matches("_h265")
                .trim_end_matches(" H265")
                .trim_end_matches(" h265");
            let has_domain = cleaned.to_lowercase().contains(".com")
                || cleaned.to_lowercase().contains(".net")
                || cleaned.to_lowercase().contains(".pl");

            let words: Vec<String> = cleaned
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_lowercase())
                .collect();

            for (i, w) in words.iter().enumerate() {
                if w.len() < 3 || w == "com" || w == "net" || w == "org" || w == "h265" {
                    continue;
                }
                let prev_is_honorific =
                    i > 0 && honorifics.contains(words[i - 1].as_str());
                let is_known_non_proper = honorifics.contains(w.as_str())
                    || function_words.contains(w.as_str())
                    || adjectives.contains(w.as_str())
                    || verbs.contains(w.as_str())
                    || common_nouns.contains(w.as_str());

                if prev_is_honorific || has_domain || !is_known_non_proper {
                    known_proper_nouns.insert(w.clone());
                }
            }
        }

        Self {
            cfg,
            honorifics,
            function_words,
            adjectives,
            verbs,
            common_nouns,
            known_proper_nouns,
        }
    }

    /// Tags a sequence of normalized lowercase tokens in context, using raw stem cues (CamelCase,
    /// domain suffixes, honorific governors, and morphological suffixes).
    pub fn tag_tokens(&self, tokens: &[String], raw_context: &str) -> Vec<PosTag> {
        let mut tags = Vec::with_capacity(tokens.len());
        let raw_has_camel = has_internal_camel_case(raw_context);

        for (i, tok) in tokens.iter().enumerate() {
            let prev_tag = if i > 0 { Some(tags[i - 1]) } else { None };
            let tag = self.classify_single(tok, prev_tag, raw_has_camel);
            tags.push(tag);
        }
        tags
    }

    fn classify_single(
        &self,
        tok: &str,
        prev_tag: Option<PosTag>,
        raw_has_camel: bool,
    ) -> PosTag {
        let lower = tok.trim();
        if lower.is_empty() {
            return PosTag::FunctionWord;
        }

        // 1. Closed-class function words (`the`, `in`, `and`, `under`, `with`, ...)
        if self.function_words.contains(lower) {
            return PosTag::FunctionWord;
        }

        // 2. Role/title honorifics (`mistress`, `goddess`, `princess`, `lady`, `miss`, ...)
        if self.honorifics.contains(lower) {
            return PosTag::Honorific;
        }

        // 3. Token immediately governed by an Honorific (`Goddess Gwen`, `Miss Tiff`, `Lady Perse`)
        //    is a Proper Noun unless it's a pure function word!
        if prev_tag == Some(PosTag::Honorific)
            && !self.adjectives.contains(lower)
            && !self.verbs.contains(lower)
        {
            return PosTag::ProperNoun;
        }

        // 4. Known lexicon Adjectives & Verbs BEFORE proper-noun fallback so words like `candid`,
        //    `new`, `vintage`, `smoking`, `laying` are accurately tagged!
        if self.adjectives.contains(lower) {
            return PosTag::Adjective;
        }
        if self.verbs.contains(lower) {
            return PosTag::Verb;
        }

        // 5. Known Proper Nouns from anchor gazetteer (`abbie`, `kendra`, `dommelia`, `weronika`)
        if self.known_proper_nouns.contains(lower) {
            return PosTag::ProperNoun;
        }

        // 6. Known concrete/domain Common Nouns (`boots`, `heels`, `interview`, `palace`, ...)
        if self.common_nouns.contains(lower) {
            return PosTag::Noun;
        }

        // 7. Morphological suffix heuristics for unseen English words:
        //    - Adverb `-ly` (except short names like `lily`, `kelly`, `emily`, `holly`, `molly`, `ally`)
        if lower.len() > 5
            && lower.ends_with("ly")
            && !matches!(lower, "emily" | "kelly" | "holly" | "molly" | "sally" | "ashley")
        {
            return PosTag::FunctionWord;
        }
        //    - Gerund / participle `-ing` (`smokng` / `inhalest` / `*ing`)
        if lower.len() > 4 && lower.ends_with("ing") {
            return PosTag::Verb;
        }
        //    - Past participle / verb `-ed`
        if lower.len() > 4 && lower.ends_with("ed") {
            return PosTag::Verb;
        }
        //    - Adjective suffixes (`-ous`, `-ive`, `-ful`, `-less`, `-able`, `-ible`, `-ish`)
        if lower.len() > 5
            && (lower.ends_with("ous")
                || lower.ends_with("ive")
                || lower.ends_with("ful")
                || lower.ends_with("less")
                || lower.ends_with("able")
                || lower.ends_with("ible")
                || lower.ends_with("ish"))
        {
            return PosTag::Adjective;
        }
        //    - Nominal suffixes (`-tion`, `-sion`, `-ment`, `-ness`, `-ity`, `-ism`, `-ist`, `-dom`, `-ship`)
        if lower.len() > 5
            && (lower.ends_with("tion")
                || lower.ends_with("sion")
                || lower.ends_with("ment")
                || lower.ends_with("ness")
                || lower.ends_with("ity")
                || lower.ends_with("ism")
                || lower.ends_with("ist")
                || lower.ends_with("dom")
                || lower.ends_with("ship"))
        {
            return PosTag::Noun;
        }

        // 8. Unseen out-of-vocabulary words: if CamelCase or alphabetic >= 3 chars, treat as
        //    ProperNoun / Noun (heavy weight, since personal names & studio brands are OOV words!).
        if raw_has_camel || lower.chars().filter(|c| c.is_alphabetic()).count() >= 3 {
            PosTag::ProperNoun
        } else {
            PosTag::Noun
        }
    }

    /// Computes the composite POS salience weight (`[0.0, 1.0]`) for a matched token slice.
    ///
    /// - Any phrase containing a `ProperNoun` (`Abbie Cat`, `Mistress Kendra`, `Dommelia`) receives
    ///   full `proper_noun_weight` (`1.00`).
    /// - Multi-token or single-token phrases without a `ProperNoun` blend `0.65 * max_weight + 0.35 * mean_weight`
    ///   so Nouns (`0.85`) weigh substantially heavier than Verbs (`0.50`), Adjectives (`0.45`),
    ///   Honorifics (`0.35`), and Function Words (`0.15`).
    pub fn phrase_pos_salience(&self, tags: &[PosTag]) -> f64 {
        if !self.cfg.enabled || tags.is_empty() {
            return 1.0;
        }
        if tags.contains(&PosTag::ProperNoun) {
            return self.cfg.proper_noun_weight;
        }
        let mut max_w = 0.0f64;
        let mut sum_w = 0.0f64;
        for &t in tags {
            let w = t.weight(&self.cfg);
            if w > max_w {
                max_w = w;
            }
            sum_w += w;
        }
        let mean_w = sum_w / (tags.len() as f64);
        (0.65 * max_w + 0.35 * mean_w).clamp(0.0, 1.0)
    }

    /// Applies POS salience weighting to a base tier score (`raw_score`):
    /// - Enclosing directory matches (`is_subdir == true`) keep their full score (`1.0000`) because
    ///   folder hierarchy is an explicit organizational signal.
    /// - Filename matches are scaled by `1.0 - nlp_influence + nlp_influence * salience` so Nouns
    ///   and Proper Nouns retain top scores while lone Adjectives/Verbs (`"Candid Smoking"`) drop
    ///   below `min_effective_score`.
    pub fn apply_pos_weight(&self, raw_score: f64, tags: &[PosTag], is_subdir: bool) -> Option<f64> {
        if !self.cfg.enabled || is_subdir {
            return Some(raw_score);
        }
        let salience = self.phrase_pos_salience(tags);
        let factor = (1.0 - self.cfg.nlp_influence) + (self.cfg.nlp_influence * salience);
        let weighted = (raw_score * factor).clamp(0.0, 1.0);
        if weighted + 1e-9 < self.cfg.min_effective_score {
            None
        } else {
            Some(weighted)
        }
    }

    /// Computes POS-weighted token-level fuzzy similarity between an alias (`alias_tokens`, `alias_tags`)
    /// and a candidate clause (`clause_tokens`, `clause_tags`).
    ///
    /// Prevents false fuzzy collisions like `"Mistress Mia"` vs `"Mistress Mira"` or `"Goddess Lena"` vs
    /// `"Goddess Lina"` where a shared low-weight Honorific (`0.35`) would otherwise mask a different
    /// high-weight Proper Noun (`1.00`)!
    pub fn weighted_token_fuzzy_similarity(
        &self,
        alias_tokens: &[String],
        alias_tags: &[PosTag],
        clause_tokens: &[String],
        clause_tags: &[PosTag],
    ) -> f64 {
        if alias_tokens.is_empty() || clause_tokens.is_empty() {
            return 0.0;
        }
        // Only comparable token counts can align token-by-token
        if alias_tokens.len().abs_diff(clause_tokens.len()) > 1 {
            return strsim::normalized_levenshtein(
                &alias_tokens.join(" "),
                &clause_tokens.join(" "),
            );
        }

        let len = alias_tokens.len().max(clause_tokens.len());
        let mut weighted_sum = 0.0f64;
        let mut total_weight = 0.0f64;

        for idx in 0..len {
            let a_tok = alias_tokens.get(idx).map(|s| s.as_str()).unwrap_or("");
            let c_tok = clause_tokens.get(idx).map(|s| s.as_str()).unwrap_or("");
            let a_w = alias_tags
                .get(idx)
                .map(|t| t.weight(&self.cfg))
                .unwrap_or(self.cfg.noun_weight);
            let c_w = clause_tags
                .get(idx)
                .map(|t| t.weight(&self.cfg))
                .unwrap_or(self.cfg.noun_weight);
            let w = a_w.max(c_w).max(0.1);
            let tok_sim = if a_tok.is_empty() || c_tok.is_empty() {
                0.0
            } else {
                strsim::normalized_levenshtein(a_tok, c_tok)
            };
            weighted_sum += w * tok_sim;
            total_weight += w;
        }

        if total_weight <= 0.0 {
            0.0
        } else {
            weighted_sum / total_weight
        }
    }
}

fn has_internal_camel_case(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    for i in 1..chars.len() {
        if chars[i - 1].is_lowercase() && chars[i].is_uppercase() {
            return true;
        }
    }
    false
}
