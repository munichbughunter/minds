//! Konkrete Secret-Detektoren für M2: bekannte Token-Formen und High-Entropy.
//!
//! Beide implementieren [`Redactor`] und liefern ausschließlich
//! [`Category::Secret`]-Funde — das *Ersetzen* bleibt allein Sache der
//! [`RedactionPipeline`](crate::RedactionPipeline). Sie ergänzen sich:
//!
//! - [`KnownTokenRedactor`] fängt **strukturierte** Zugangsdaten mit stabilem
//!   Präfix und fester Form (AWS-Access-Key, GitHub-/GitLab-Token, Slack, Google,
//!   Stripe, npm, JWT, PEM-Private-Keys). Präzise, praktisch fehlalarmfrei.
//! - [`HighEntropyRedactor`] ist das **Auffangnetz** für generische, hoch-
//!   entropische Blobs (base64/hex) *ohne* bekanntes Präfix. Unschärfer, dafür
//!   formunabhängig.
//!
//! Feuern beide auf dasselbe Geheimnis, führt die Pipeline die überlappenden
//! Funde zusammen — einmal ersetzt, einmal gezählt (Dedup gratis).
//!
//! # Architektur des Token-Detektors (Vorfilter + Shape-Validierung)
//!
//! Der industrieübliche Aufbau (Gitleaks, TruffleHog): erst ein **billiger
//! Multi-Pattern-Vorfilter** über alle bekannten Präfixe, dann eine **strikte
//! Regex** nur an den Treffern.
//!
//! 1. **Vorfilter — [`aho_corasick`]:** ein einziger Durchlauf findet *alle*
//!    Präfixe gleichzeitig (`ghp_`, `AKIA`, `xoxb-`, `-----BEGIN ` …), statt den
//!    Text pro Muster erneut zu scannen. Das sortiert 99 % irrelevanten Text
//!    aus, bevor überhaupt eine Regex läuft.
//! 2. **Shape-Validierung — [`regex`]:** nur ab einer Präfix-Position prüft die
//!    zum Präfix gehörende, am Anfang verankerte (`\A`) Regex die volle Token-
//!    Form. Schlägt sie fehl (`-----BEGIN CERTIFICATE-----`, ein nacktes `AKIA`),
//!    entsteht kein Fund.
//!
//! **Zur ReDoS-Sorge:** Rusts [`regex`] ist eine endliche-Automaten-Engine
//! (RE2-Linie) mit **garantiert linearer** Laufzeit — die katastrophale
//! Backtracking-Explosion, die PCRE/JS/Python treffen kann, ist hier
//! ausgeschlossen. Trotzdem sind alle Muster strikt **begrenzt** (feste Längen,
//! gedeckelte Wiederholungen `{m,n}`): das hält die Funde präzise und die
//! Match-Länge kalkulierbar, unabhängig von der Engine.
//!
//! Nicht mmap-basiert: Minds bereinigt **Session-Text im Speicher** (Turn-Texte,
//! Tool-Argumente), keine großen Dateien auf der Platte — es gibt nichts zu
//! memory-mappen.

use aho_corasick::{AhoCorasick, MatchKind};
use regex::Regex;

use crate::redactor::{Category, Finding, Redactor};

// ---------------------------------------------------------------------------
// Bekannte Token-Formen
// ---------------------------------------------------------------------------

/// Eine Regel für eine bekannte Token-Form: feste Präfixe für den Vorfilter und
/// eine am Anfang verankerte Regex für die Shape-Validierung.
struct TokenRule {
    /// Stabiler Bezeichner der Form (für spätere Per-Regel-Audits und
    /// [`KnownTokenRedactor::covered_formats`]).
    name: &'static str,
    /// Feste Präfixe, die der Aho-Corasick-Vorfilter sucht. Ein Treffer ist nur
    /// *Kandidat* — die Regel gilt erst, wenn `pattern` ab der Trefferposition
    /// matcht. Mehrere Präfixe teilen sich dieselbe Regex.
    prefixes: &'static [&'static str],
    /// Am Anfang verankertes (`\A`), striktes, längenbegrenztes Muster, das die
    /// volle Token-Form *inklusive* Präfix beschreibt.
    pattern: &'static str,
    /// Ob der Präfix am **Wortanfang** stehen muss.
    ///
    /// Für Präfixe, die auch als Wortende vorkommen: `sk-` steht in `disk-`,
    /// `task-`, `risk-`; `SG.` in `MSG.`. Ohne diese Prüfung wird aus
    /// `disk-<lange Build-ID>` ein Fund, der die halbe Zeile schwärzt.
    ///
    /// Bewusst **nicht** für alle Regeln: Ein Token, das ohne Trenner in einem
    /// base64-Blob steckt, soll weiter gefunden werden. Die Prüfung kostet
    /// Recall und wird nur dort bezahlt, wo sie nachweislich Fehlalarme
    /// verhindert.
    needs_word_start: bool,
}

/// Die Regeltabelle. Bewusst kuratiert auf hochwertige, fehlalarmarme Formen mit
/// klarer Struktur; das generische Auffangnetz übernimmt [`HighEntropyRedactor`].
///
/// Jedes `pattern` ist mit `\A` verankert und in der Länge gedeckelt (keine
/// offenen `+`/`*` ohne Obergrenze), damit Funde präzise begrenzt bleiben.
///
/// # Bewusst *nicht* enthalten: der kontextfreie AWS-Secret-Key
///
/// Ein AWS Secret Access Key ist 40 Zeichen base64 **ohne Präfix** — es gibt
/// nichts, woran ein Vorfilter greifen könnte. Die naheliegende Regel
/// `\b[A-Za-z0-9/+=]{40}\b` träfe jeden **Git-SHA**, und minds ist ein
/// Git-Werkzeug: Commit-Hashes stehen in praktisch jedem Transkript. Der
/// Record wäre unbrauchbar.
///
/// Ungeschützt ist der Key deshalb nicht — er wird von den anderen Schichten
/// getragen: `.aws/credentials` ist über [`crate::secretfile`] gemauert, die
/// beschriftete Form (`aws_secret_access_key=…`) fängt der
/// [`KeyValueRedactor`](crate::KeyValueRedactor), und der kontextfreie Fall
/// liegt beim Entropie-Netz. Dort bleibt ein kleiner Rest: base64-Läufe dieser
/// Länge liegen nur selten unter der Schwelle von 4.5 bit/Zeichen.
const RULES: &[TokenRule] = &[
    // AWS Access Key ID: AKIA/ASIA + 16 Großbuchstaben/Ziffern.
    TokenRule {
        name: "aws-access-key-id",
        prefixes: &["AKIA", "ASIA"],
        pattern: r"\A(?:AKIA|ASIA)[0-9A-Z]{16}",
        needs_word_start: false,
    },
    // GitHub-Token (PAT/OAuth/App): ghp_/gho_/ghu_/ghs_/ghr_ + 36 alnum.
    TokenRule {
        name: "github-token",
        prefixes: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        pattern: r"\Agh[pousr]_[0-9A-Za-z]{36}",
        needs_word_start: false,
    },
    // GitHub Fine-grained PAT: github_pat_ + 22 alnum + _ + 59 alnum.
    TokenRule {
        name: "github-fine-grained-pat",
        prefixes: &["github_pat_"],
        pattern: r"\Agithub_pat_[0-9A-Za-z]{22}_[0-9A-Za-z]{59}",
        needs_word_start: false,
    },
    // Anthropic API Key: sk-ant- + **Typ-Sektion** + Rest. Für dieses Projekt
    // die wichtigste Form überhaupt — die Testgruppe arbeitet mit Claude Code,
    // diese Keys stehen buchstäblich in den Sessions, die minds archiviert.
    //
    // Die Typ-Sektion (`api03`, `oat01`, `sid01`, `adm01`) ist nicht Kosmetik,
    // sondern trennt Key von Prosa: Ohne sie matcht jede Kebab-Kette hinter
    // `sk-ant-`, und ausgerechnet in Claude-Code-Sessions wird über Keys
    // *geredet* — `sk-ant-api-keys-rotieren-anleitung` hätte den Record
    // zerstört.
    TokenRule {
        name: "anthropic-api-key",
        prefixes: &["sk-ant-"],
        pattern: r"\Ask-ant-[a-z]{3}[0-9]{2}-[0-9A-Za-z_-]{24,256}\b",
        needs_word_start: false,
    },
    // OpenAI: die neuen typisierten Keys (`sk-proj-`, `sk-svcacct-`,
    // `sk-admin-`) und die alte Form `sk-` + 48 alnum.
    //
    // Das nackte `sk-` ist der Grund, warum der Vorfilter **überlappend**
    // suchen muss: Es ist ein Präfix von `sk-ant-`. Meldete der Automat an
    // einer Position nur den kürzesten Treffer, liefe ein Anthropic-Key in
    // diese Regel, scheiterte an ihr — und wäre nie geprüft worden.
    //
    // `needs_word_start`, weil `sk-` auch am Ende gewöhnlicher Wörter steht:
    // `disk-`, `task-`, `risk-`, `desk-`. Mit einer langen Build-ID dahinter
    // wäre jedes davon ein Fehlalarm.
    TokenRule {
        name: "openai-api-key",
        prefixes: &["sk-proj-", "sk-svcacct-", "sk-admin-", "sk-"],
        pattern: r"\Ask-(?:(?:proj|svcacct|admin)-[0-9A-Za-z_-]{20,256}|[0-9A-Za-z]{48})\b",
        needs_word_start: true,
    },
    // GitLab — und hier lohnt die Vollständigkeit besonders, weil Produkt und
    // Testgruppe auf GitLab zielen. Neben dem PAT gibt es eine ganze Familie:
    // Runner (`glrt-`), Deploy (`gldt-`), **CI/CD-Job-Token** (`glcbt-`),
    // Pipeline-Trigger (`glptt-`), Feed (`glft-`), Incoming Mail (`glimt-`),
    // Agent für Kubernetes (`glagent-`), SCIM (`glsoat-`).
    //
    // `glcbt-` ist der wahrscheinlichste Token in einer Agent-Session mit
    // GitLab-CI überhaupt, und `glptt-` ist der gefährlichste: 40 Hex-Zeichen
    // liegen mit H ≈ 4.0 **strukturell** unter der Entropie-Schwelle von 4.5 —
    // das Auffangnetz fängt diese Form nie, egal wie lang sie ist. Ohne Regel
    // steht der Token im Klartext im Envelope.
    TokenRule {
        name: "gitlab-pat",
        prefixes: &[
            "glpat-", "glrt-", "gldt-", "glcbt-", "glptt-", "glft-", "glimt-", "glagent-",
            "glsoat-",
        ],
        pattern: r"\Agl(?:pat|rt|dt|cbt|ptt|ft|imt|agent|soat)-[0-9A-Za-z_-]{20,128}\b",
        needs_word_start: false,
    },
    // Slack-Token: Bot/User/App-Level, Client (`xoxc-`) und das zugehörige
    // `d`-Cookie (`xoxd-`). Die beiden letzten sind bei Browser-Auth **immer
    // ein Paar** — eins allein ist nutzlos, beide zusammen sind eine
    // vollständige Session-Übernahme.
    //
    // Der Cap ist bewusst großzügig: Reale `xoxc-`/`xoxp-` haben die Form
    // `<team>-<user>-<session>-<64 hex>` und liegen bei ~106 Zeichen. Ein zu
    // kleiner Cap schneidet den Fund ab und lässt den Schwanz stehen — und
    // gerade bei hex-lastigen Läufen rettet das Entropie-Netz nicht. Hinter
    // `xox…-` steht nie Prosa, Über-Redaktion kostet hier also nichts.
    //
    // `xapp-` bekommt die Ziffern-Sektion, weil es sonst jeden Projektnamen
    // trifft (`xapp-android-build-pipeline-nightly`).
    TokenRule {
        name: "slack-token",
        prefixes: &[
            "xoxb-", "xoxa-", "xoxp-", "xoxr-", "xoxs-", "xoxe-", "xoxc-", "xoxd-", "xapp-",
        ],
        pattern: r"\A(?:xox[baprsecd]-|xapp-[0-9]-)[0-9A-Za-z-]{10,255}\b",
        needs_word_start: false,
    },
    // Legacy-Runner-Registrierungstoken: fester Präfix `GR1348941`.
    TokenRule {
        name: "gitlab-runner-registration",
        prefixes: &["GR1348941"],
        pattern: r"\AGR1348941[0-9A-Za-z_-]{20,64}\b",
        needs_word_start: false,
    },
    // SendGrid API Key: SG. + Key-Id + . + Secret. `needs_word_start`, weil
    // der Präfix sonst mitten im Wort feuert (`MSG.`).
    TokenRule {
        name: "sendgrid-key",
        prefixes: &["SG."],
        pattern: r"\ASG\.[0-9A-Za-z_-]{16,32}\.[0-9A-Za-z_-]{16,64}\b",
        needs_word_start: true,
    },
    // Google API Key: AIza + 35 Zeichen.
    TokenRule {
        name: "google-api-key",
        prefixes: &["AIza"],
        pattern: r"\AAIza[0-9A-Za-z_-]{35}",
        needs_word_start: false,
    },
    // Stripe Live Secret/Restricted Key: sk_live_/rk_live_ + alnum. Neuere
    // Keys reichen deutlich über die ursprünglichen 64 Zeichen hinaus; ohne
    // den größeren Cap deckt der Fund nur den Anfang ab und der Rest hängt am
    // Entropie-Netz.
    TokenRule {
        name: "stripe-live-key",
        prefixes: &["sk_live_", "rk_live_"],
        pattern: r"\A(?:sk|rk)_live_[0-9A-Za-z]{16,128}",
        needs_word_start: false,
    },
    // npm Access Token: npm_ + 36 alnum.
    TokenRule {
        name: "npm-token",
        prefixes: &["npm_"],
        pattern: r"\Anpm_[0-9A-Za-z]{36}",
        needs_word_start: false,
    },
    // JSON Web Token: header.payload.signature, jede Sektion base64url. Der
    // Header beginnt fast immer mit `eyJ` (base64url von `{"`). Längen gedeckelt.
    TokenRule {
        name: "jwt",
        prefixes: &["eyJ"],
        pattern: r"\AeyJ[0-9A-Za-z_=-]{10,4000}\.[0-9A-Za-z_=-]{6,4000}\.[0-9A-Za-z_=-]{6,4000}",
        needs_word_start: false,
    },
    // PEM Private-Key-Block: BEGIN…END, optionaler Key-Typ, base64/Whitespace-
    // Körper (gedeckelt). Der Vorfilter „-----BEGIN " matcht auch CERTIFICATE —
    // die Regex verwirft das, weil sie „PRIVATE KEY" verlangt.
    //
    // Der Backslash in der Körperklasse ist der **JSON-Fall**: Tool-Argumente
    // liegen serialisiert vor, ein Key steht dort als
    // `-----BEGIN RSA PRIVATE KEY-----\nMII…` mit *literalem* Backslash-n.
    // Ohne `\\` bricht die Klasse am ersten Escape ab, die Regel matcht nicht,
    // und übrig bleibt, was das Entropie-Netz einzeln erwischt — also gerade
    // die kurze Schlusszeile **nicht**, denn die liegt unter 32 Zeichen.
    //
    // Bewusst als Zeichenklasse und nicht als Alternation `(?:\\r|\\n|[…])`:
    // Die Zähl-Wiederholung `{0,10000}` wird vom Compiler entrollt, und eine
    // Alternation je Kopie vervielfacht den Automaten (vgl. die Begründung an
    // [`crate::assignment::VALUE`]). Ein Zeichen mehr in der Klasse kostet
    // dagegen nichts. Breiter wird die Regel dadurch kaum: Sie bleibt beidseitig
    // an den BEGIN-/END-Markern verankert.
    TokenRule {
        name: "private-key-pem",
        prefixes: &["-----BEGIN "],
        pattern: concat!(
            r"\A-----BEGIN (?:RSA |DSA |EC |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY-----",
            r"[A-Za-z0-9+/=\s\\]{0,10000}?",
            r"-----END (?:RSA |DSA |EC |OPENSSH |PGP |ENCRYPTED )?PRIVATE KEY-----",
        ),
        needs_word_start: false,
    },
];

/// Detektor für **bekannte Token-Formen**: Aho-Corasick-Vorfilter über alle
/// Präfixe, dann strikte, verankerte Regex-Validierung je Treffer.
///
/// Baut Automat und Regexe **einmal** bei der Konstruktion; [`scan`](Redactor::scan)
/// ist danach allokationsarm. Die Muster sind Konstanten und in den Tests
/// abgedeckt — ein Bau-/Kompilierfehler wäre ein Programmierfehler, deshalb
/// `expect` in [`new`](Self::new) statt eines Laufzeit-`Result`.
pub struct KnownTokenRedactor {
    /// Vorfilter über alle Präfixe aller Regeln (in Reihenfolge von `RULES`).
    prefilter: AhoCorasick,
    /// Bildet die *Präfix*-Pattern-ID des Vorfilters auf den Index der Regel in
    /// `rules` ab (mehrere Präfixe je Regel ⇒ mehrere Einträge zeigen auf denselben Index).
    prefix_to_rule: Vec<usize>,
    /// Kompilierte Regeln, gleiche Reihenfolge wie `RULES`.
    rules: Vec<CompiledRule>,
}

/// Eine Regel nach dem Kompilieren: Name plus verankerte Regex.
struct CompiledRule {
    name: &'static str,
    re: Regex,
}

impl KnownTokenRedactor {
    /// Baut den Vorfilter aus allen Präfixen und kompiliert eine Regex je Regel.
    pub fn new() -> Self {
        let mut prefixes: Vec<&'static str> = Vec::new();
        let mut prefix_to_rule: Vec<usize> = Vec::new();
        let mut rules: Vec<CompiledRule> = Vec::with_capacity(RULES.len());

        for (idx, rule) in RULES.iter().enumerate() {
            for &prefix in rule.prefixes {
                prefixes.push(prefix);
                prefix_to_rule.push(idx);
            }
            let re = Regex::new(rule.pattern).expect("konstantes Token-Muster muss kompilieren");
            rules.push(CompiledRule {
                name: rule.name,
                re,
            });
        }

        // `Standard` ist Pflicht, nicht Geschmackssache: Nur diese Semantik
        // erlaubt die überlappende Suche in [`Redactor::scan`], und die
        // brauchen wir, seit ein Präfix Präfix eines anderen sein kann
        // (`sk-` und `sk-ant-`).
        let prefilter = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(&prefixes)
            .expect("konstante Präfix-Liste muss bauen");

        Self {
            prefilter,
            prefix_to_rule,
            rules,
        }
    }

    /// Die Namen aller abgedeckten Token-Formen (für Doku, Diagnose und den
    /// späteren Per-Regel-Audit).
    pub fn covered_formats(&self) -> Vec<&'static str> {
        self.rules.iter().map(|r| r.name).collect()
    }
}

impl Default for KnownTokenRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor for KnownTokenRedactor {
    fn name(&self) -> &str {
        "known-token"
    }

    fn scan(&self, text: &str) -> Vec<Finding> {
        let mut out = Vec::new();
        // **Überlappend** suchen, nicht `find_iter`: Ist ein Präfix Präfix
        // eines anderen (`sk-` und `sk-ant-`), meldet die nicht-überlappende
        // Suche an einer Position nur den kürzesten Treffer. Der Anthropic-Key
        // liefe dann in die OpenAI-Regel, scheiterte an ihr — und die
        // zuständige Regel wäre nie gefragt worden. Jedes Präfix-Vorkommen ist
        // ein Kandidat; welche Regel greift, entscheidet die Regex danach.
        // Überlappende *Funde* führt die Pipeline anschließend zusammen.
        for m in self.prefilter.find_overlapping_iter(text) {
            let start = m.start();
            let rule_spec = &RULES[self.prefix_to_rule[m.pattern().as_usize()]];
            // Präfixe, die auch Wortende sein können (`sk-` in `disk-`, `SG.`
            // in `MSG.`), nur am Wortanfang zulassen. Alle Präfixe sind ASCII,
            // deshalb genügt der Byte-Blick zurück.
            if rule_spec.needs_word_start
                && start > 0
                && text.as_bytes()[start - 1].is_ascii_alphanumeric()
            {
                continue;
            }
            let rule = &self.rules[self.prefix_to_rule[m.pattern().as_usize()]];
            // `\A` verankert am Anfang des Slices — ein Treffer beginnt also
            // zwingend bei `start`. `start` liegt auf einer UTF-8-Grenze, weil
            // alle Präfixe ASCII sind.
            if let Some(mat) = rule.re.find(&text[start..]) {
                out.push(Finding::new(Category::Secret, start, start + mat.end()));
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// High-Entropy
// ---------------------------------------------------------------------------

/// Standard-Mindestlänge eines Kandidaten-Laufs für [`HighEntropyRedactor`].
///
/// Bei 32 Zeichen ist die maximal mögliche Shannon-Entropie `log2(32) = 5.0`
/// bit/Zeichen — genug Kopfraum über [`DEFAULT_ENTROPY_BITS`]. (Die Entropie
/// eines Laufs ist durch `log2(Länge)` gedeckelt; zu kurze Läufe *können* die
/// Schwelle gar nicht erreichen und würden nur Fehlalarme produzieren.)
pub const DEFAULT_MIN_LEN: usize = 32;

/// Standard-Entropieschwelle in bit/Zeichen.
///
/// `4.5` liegt bewusst **über** dem Hex-Maximum (Alphabet 16 ⇒ höchstens 4.0
/// bit) und über den gemessenen Entropien typischer Nicht-Secrets: Git-SHAs
/// (~3.8), MD5 (~3.4), UUIDs (~3.4), Pfade und kebab/snake-Bezeichner (~3.8–4.3).
/// Zufälliges base64 liegt ab ~40 Zeichen bei ~4.6–5.5 und bleibt hängen. Der
/// Detektor ist damit **präzisions- vor recall-orientiert**: prefixlose 32-
/// Zeichen-Blobs sind je nach Zufall Grenzfälle (~4.2–4.9) und können entgehen —
/// die allermeisten Secrets tragen ohnehin ein bekanntes Präfix und fallen
/// [`KnownTokenRedactor`] zu. Feinjustierung mit FP/FN-Korpus folgt im
/// Test-Commit von M2.
pub const DEFAULT_ENTROPY_BITS: f64 = 4.5;

/// Generischer Auffang-Detektor: markiert zusammenhängende Läufe aus
/// „secret-typischen" Zeichen (base64/base64url-Alphabet: `A–Z a–z 0–9 + / _ -`),
/// die lang genug **und** hoch-entropisch genug sind.
///
/// Bewusst formunabhängig — er kennt keine Präfixe. Das macht ihn zum Netz
/// hinter [`KnownTokenRedactor`], aber auch unschärfer; deshalb sind Länge und
/// Schwelle konservativ vorbelegt und über [`with_params`](Self::with_params)
/// justierbar.
pub struct HighEntropyRedactor {
    min_len: usize,
    min_entropy_bits: f64,
}

impl HighEntropyRedactor {
    /// Mit den Standardwerten [`DEFAULT_MIN_LEN`] / [`DEFAULT_ENTROPY_BITS`].
    pub fn new() -> Self {
        Self::with_params(DEFAULT_MIN_LEN, DEFAULT_ENTROPY_BITS)
    }

    /// Mit eigener Mindestlänge und Entropieschwelle (bit/Zeichen).
    ///
    /// Sinnvoll ist `min_entropy_bits < log2(min_len)`; darüber kann kein Lauf
    /// dieser Länge die Schwelle erreichen (die Entropie ist durch `log2(Länge)`
    /// gedeckelt).
    pub fn with_params(min_len: usize, min_entropy_bits: f64) -> Self {
        Self {
            min_len,
            min_entropy_bits,
        }
    }
}

impl Default for HighEntropyRedactor {
    fn default() -> Self {
        Self::new()
    }
}

impl Redactor for HighEntropyRedactor {
    fn name(&self) -> &str {
        "high-entropy"
    }

    fn scan(&self, text: &str) -> Vec<Finding> {
        let bytes = text.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if is_secret_char(bytes[i]) {
                let start = i;
                while i < bytes.len() && is_secret_char(bytes[i]) {
                    i += 1;
                }
                // Alle Zeichen des Laufs sind ASCII ⇒ `start` und `i` liegen auf
                // UTF-8-Grenzen; der Slice ist gültig.
                let run = &text[start..i];
                if run.len() >= self.min_len && shannon_entropy(run) >= self.min_entropy_bits {
                    out.push(Finding::new(Category::Secret, start, i));
                }
            } else {
                // Kein Kandidatenzeichen (inkl. jedes Nicht-ASCII-Bytes eines
                // Mehrbyte-Zeichens) — beendet den Lauf, nie in einen Fund gezogen.
                i += 1;
            }
        }
        out
    }
}

/// `true` für Zeichen des base64/base64url-Alphabets (ohne Padding): `A–Z a–z
/// 0–9` plus `+ / _ -`.
///
/// Das Padding `=` gehört **bewusst nicht** dazu: es steht nur am Ende eines
/// base64-Werts (trägt ~0 Entropie) und ist zugleich der allgegenwärtige
/// Zuweisungsoperator. Wäre `=` Teil des Laufs, verschmölze `key=<secret>` zu
/// *einem* Lauf und der Schlüsselname geriete mit in den Fund. Als Trenner
/// zerlegt `=` sauber in Schlüssel und Wert; die weggelassene `=`-Endung ist
/// nicht sensibel.
const fn is_secret_char(b: u8) -> bool {
    matches!(
        b,
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' | b'_' | b'-'
    )
}

/// Shannon-Entropie von `s` in **bit pro Zeichen**, byte-basiert.
///
/// Nur auf ASCII-Läufen aufgerufen (ein Byte = ein Zeichen), daher ist die
/// Byte-Häufigkeit die Zeichen-Häufigkeit. `H = -Σ p·log2(p)`; für den leeren
/// String `0.0`. Das `f64` ist rein intern — es geht nie ins Envelope (dessen
/// Kanonisierung Floats ablehnt), sondern nur in den Schwellenvergleich.
fn shannon_entropy(s: &str) -> f64 {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 0.0;
    }
    let mut freq = [0u32; 256];
    for &b in bytes {
        freq[b as usize] += 1;
    }
    let len = bytes.len() as f64;
    let mut h = 0.0;
    for &count in &freq {
        if count > 0 {
            let p = f64::from(count) / len;
            h -= p * p.log2();
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::RedactionPipeline;

    /// Bequemer Durchlauf durch eine Pipeline mit genau einem Detektor.
    fn redact_with<R: Redactor + 'static>(r: R, text: &str) -> (String, u32) {
        let out = RedactionPipeline::new().with(r).redact(text);
        (out.text, out.counts.secrets)
    }

    // --- Bekannte Token-Formen: Positiv-Fälle ---------------------------------

    #[test]
    fn detects_aws_access_key_id() {
        let (text, n) = redact_with(
            KnownTokenRedactor::new(),
            "aws_key = AKIAIOSFODNN7EXAMPLE done",
        );
        assert_eq!(text, "aws_key = [redacted:secret] done");
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_github_token() {
        let token = format!("ghp_{}", "A1b2C3d4E5".repeat(3) + "ABCDEF");
        assert_eq!(token.len(), 4 + 36);
        let (text, n) = redact_with(KnownTokenRedactor::new(), &format!("token={token}"));
        assert_eq!(text, "token=[redacted:secret]");
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_gitlab_pat() {
        // GitLab-fokussiert: glpat- + 20 Zeichen.
        let token = "glpat-ABCDEFGHIJ1234567890";
        assert_eq!(token.len(), 6 + 20);
        let (text, n) = redact_with(KnownTokenRedactor::new(), &format!("GITLAB_TOKEN={token}"));
        assert_eq!(text, "GITLAB_TOKEN=[redacted:secret]");
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_slack_token() {
        let token = concat!("xoxb", "-123456789012-abcdefghijklmnopqrstuvwx");
        let (_, n) = redact_with(KnownTokenRedactor::new(), token);
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_google_api_key() {
        let token = format!("AIza{}", "A1b2C3d4E5".repeat(3) + "ABCDE");
        assert_eq!(token.len(), 4 + 35);
        let (_, n) = redact_with(KnownTokenRedactor::new(), &token);
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_stripe_live_key() {
        let token = format!("sk_live_{}", "A1b2C3d4E5".repeat(2) + "ABCD");
        assert!(token.len() >= 8 + 16);
        let (_, n) = redact_with(KnownTokenRedactor::new(), &token);
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_npm_token() {
        let token = format!("npm_{}", "A1b2C3d4E5".repeat(3) + "ABCDEF");
        assert_eq!(token.len(), 4 + 36);
        let (_, n) = redact_with(KnownTokenRedactor::new(), &token);
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.\
                   eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4ifQ.\
                   dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let (text, n) = redact_with(KnownTokenRedactor::new(), jwt);
        assert_eq!(text, "[redacted:secret]");
        assert_eq!(n, 1);
    }

    #[test]
    fn detects_pem_private_key_block() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\n\
                   MIIBogIBAAJBAKj34GkxFhD90vcNLYLInFEX6Ppy1tPf9Cnzj4p4WGeKLs1Pt8Q\n\
                   uKUpRKfFLfRYC9AIKjbJTWit+CzZiMbAgMBAAE=\n\
                   -----END RSA PRIVATE KEY-----";
        let (text, n) = redact_with(KnownTokenRedactor::new(), pem);
        assert_eq!(text, "[redacted:secret]");
        assert_eq!(n, 1);
    }

    // --- Bekannte Token-Formen: Negativ-Fälle (kein Fehlalarm) ----------------

    #[test]
    fn bare_prefix_without_valid_shape_is_ignored() {
        // Präfix da, aber Form falsch: AKIA zu kurz, ghp_ zu kurz.
        let (text, n) = redact_with(KnownTokenRedactor::new(), "AKIA und ghp_kurz");
        assert_eq!(text, "AKIA und ghp_kurz");
        assert_eq!(n, 0);
    }

    #[test]
    fn pem_certificate_is_not_a_private_key() {
        // Der Vorfilter matcht „-----BEGIN ", die Regex verlangt aber PRIVATE KEY.
        let cert = "-----BEGIN CERTIFICATE-----\nMIIB...\n-----END CERTIFICATE-----";
        let (text, n) = redact_with(KnownTokenRedactor::new(), cert);
        assert_eq!(text, cert);
        assert_eq!(n, 0);
    }

    #[test]
    fn prose_without_tokens_is_untouched() {
        let prose = "Der Reviewer liest die Absicht, nicht nur den Diff.";
        let (text, n) = redact_with(KnownTokenRedactor::new(), prose);
        assert_eq!(text, prose);
        assert_eq!(n, 0);
    }

    // --- Bekannte Token-Formen: Struktur & Ränder -----------------------------

    #[test]
    fn multiple_tokens_all_detected() {
        let text = "a AKIAIOSFODNN7EXAMPLE b glpat-ABCDEFGHIJ1234567890 c";
        let (out, n) = redact_with(KnownTokenRedactor::new(), text);
        assert_eq!(out, "a [redacted:secret] b [redacted:secret] c");
        assert_eq!(n, 2);
    }

    #[test]
    fn token_between_multibyte_text_keeps_offsets() {
        // é (2 Byte) und 🦀 (4 Byte) rund um das Token: Offsets müssen stimmen.
        let text = "café🦀AKIAIOSFODNN7EXAMPLE🦀café";
        let (out, n) = redact_with(KnownTokenRedactor::new(), text);
        assert_eq!(out, "café🦀[redacted:secret]🦀café");
        assert_eq!(n, 1);
    }

    #[test]
    fn findings_are_secret_category() {
        let f = KnownTokenRedactor::new().scan("AKIAIOSFODNN7EXAMPLE");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].category, Category::Secret);
        assert_eq!(f[0].start, 0);
        assert_eq!(f[0].end, 20);
    }

    #[test]
    fn known_token_name_is_stable() {
        assert_eq!(KnownTokenRedactor::new().name(), "known-token");
    }

    #[test]
    fn covered_formats_lists_the_rules() {
        let formats = KnownTokenRedactor::new().covered_formats();
        assert!(formats.contains(&"aws-access-key-id"));
        assert!(formats.contains(&"gitlab-pat"));
        assert!(formats.contains(&"private-key-pem"));
        assert!(formats.contains(&"anthropic-api-key"));
        assert_eq!(formats.len(), RULES.len());
    }

    #[test]
    fn a_prefix_that_starts_with_another_prefix_is_still_a_candidate() {
        // `sk-` ist ein Präfix von `sk-ant-`. Meldete der Vorfilter an einer
        // Position nur den kürzesten Treffer, liefe ein Anthropic-Key in die
        // OpenAI-Regel, scheiterte dort — und wäre nie geprüft worden.
        //
        // Der Test läuft **ohne** Entropie-Netz: Sonst bewiese er nur, dass
        // irgendein Detektor zugegriffen hat, nicht die Token-Regel.
        let key = concat!(
            "sk-ant",
            "-api03-A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4Y5z6"
        );
        let (text, n) = redact_with(
            KnownTokenRedactor::new(),
            &format!("Fehler: {key} ungueltig"),
        );
        assert_eq!(n, 1, "kein known-token-Fund fuer sk-ant:\n{text}");
        assert!(!text.contains("api03"), "Key steht noch im Text:\n{text}");
        assert!(text.starts_with("Fehler: "), "Kontext weg:\n{text}");
    }

    #[test]
    fn the_new_token_formats_are_caught_without_the_entropy_net() {
        // Akzeptanzkriterium aus #33: Die Abdeckung darf nicht am Netz hängen.
        // Ein Key in einer Fehlermeldung hat keine Zuweisung, die der
        // Key-Value-Detektor sehen könnte — hier zählt allein die Token-Regel.
        for (form, token) in [
            (
                "openai-proj",
                concat!("sk-proj", "-A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0"),
            ),
            (
                "openai-legacy",
                "sk-A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v2W3x4",
            ),
            ("gitlab-runner", concat!("glrt", "-A1b2C3d4E5f6G7h8I9j0K1")),
            ("gitlab-deploy", concat!("gldt", "-A1b2C3d4E5f6G7h8I9j0K1")),
            // Der wahrscheinlichste Token einer GitLab-CI-Session.
            (
                "gitlab-ci-job",
                concat!("glcbt", "-K7pR2xQm9vTzL4nB6wYdA1b2"),
            ),
            (
                "gitlab-ci-job-routable",
                concat!("glcbt", "-1_K7pR2xQm9vTzL4nB6wYd"),
            ),
            // 40 Hex: H ≈ 4.0, das Entropie-Netz kann hier konstruktions-
            // bedingt nicht helfen. Ohne eigene Regel bliebe der Token stehen.
            (
                "gitlab-trigger",
                concat!("glptt", "-a3f9c21b8e4d70561fa2b93c8d7e015426ab9cd3"),
            ),
            (
                "gitlab-agent",
                concat!("glagent", "-K7pR2xQm9vTzL4nB6wYdA1b2"),
            ),
            ("gitlab-feed", concat!("glft", "-K7pR2xQm9vTzL4nB6wYdA1b2")),
            (
                "gitlab-runner-registration",
                concat!("GR13489", "41K7pR2xQm9vTzL4nB6wYd"),
            ),
            // Das Gegenstück zu xoxc- bei Browser-Auth; ebenfalls Hex.
            (
                "slack-d-cookie",
                concat!(
                    "xoxd",
                    "-a3f9c21b8e4d70561fa2b93c8d7e015426ab9cd3f81e2a740b6c359d8e1f0a2b"
                ),
            ),
            // Volle reale Länge (~106 Zeichen nach dem Präfix): Ein zu kleiner
            // Cap schnitte den Fund ab und ließe den Schwanz stehen.
            (
                "slack-client-full-length",
                concat!(
                    "xoxc",
                    "-2345678901234-2345678901234-2345678901234-a3f9c21b8e4d70561fa2b93c8d7e015426ab9cd3f81e2a740b6c359d8e1f0a2b"
                ),
            ),
            (
                "anthropic-oauth",
                concat!(
                    "sk-ant",
                    "-oat01-AbCdEf0123456789_-AbCdEf0123456789AbCdEf0123456789AbCdEf0123456789AAA"
                ),
            ),
            (
                "slack-client",
                concat!("xoxc", "-1234567890-1234567890-A1b2C3d4E5f6G7h8I9j0K1l2"),
            ),
            (
                "slack-app-level",
                concat!("xapp", "-1-A01BCDEFGHI-1234567890123-A1b2C3d4E5f6G7h8I9j0"),
            ),
            (
                "sendgrid",
                concat!(
                    "SG",
                    ".A1b2C3d4E5f6G7h8I9j0K1.A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0"
                ),
            ),
        ] {
            let (text, n) = redact_with(
                KnownTokenRedactor::new(),
                &format!("Fehler: {token} abgelehnt"),
            );
            assert_eq!(n, 1, "{form}: kein known-token-Fund:\n{text}");
            assert_eq!(
                text, "Fehler: [redacted:secret] abgelehnt",
                "{form}: Fund deckt nicht den ganzen Token ab"
            );
        }
    }

    #[test]
    fn token_prefixes_that_are_also_word_endings_do_not_fire() {
        // `sk-` steht am Ende gewöhnlicher Wörter, `SG.` mitten in `MSG.`.
        // Mit einer langen Id dahinter wäre jedes davon ein Fehlalarm, der
        // eine halbe Zeile schwärzt — der Record wäre kaputt, ohne dass
        // irgendetwas geschützt würde.
        for text in [
            "Build task-0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6071 fertig",
            "disk-aaaaaaaabbbbccccddddeeeeeeeeeeeeaaaaaaaabbbbcccc gemountet",
            "MSG.AbCdEfGhIjKlMnOpQr.AbCdEfGhIjKlMnOpQrStUv im Log",
        ] {
            let (out, n) = redact_with(KnownTokenRedactor::new(), text);
            assert_eq!(n, 0, "Fehlalarm auf {text:?}:\n{out}");
            assert_eq!(out, text);
        }
    }

    #[test]
    fn talking_about_keys_does_not_redact_the_sentence() {
        // In Claude-Code-Sessions wird über Keys *geredet*. Ohne die
        // Typ-Sektion im Anthropic-Muster und die Ziffern-Sektion bei `xapp-`
        // matchte jede Kebab-Kette hinter dem Präfix.
        for text in [
            "Doku: https://docs.example.test/sk-ant-api-keys-rotieren-anleitung",
            "Branch feature/sk-ant-key-rotation-und-fehlerbehandlung gemergt",
            "Job xapp-android-build-pipeline-nightly ist rot",
            "Ordner xapp-frontend-komponenten-bibliothek angelegt",
        ] {
            let (out, n) = redact_with(KnownTokenRedactor::new(), text);
            assert_eq!(n, 0, "Fehlalarm auf {text:?}:\n{out}");
        }
    }

    // --- High-Entropy ---------------------------------------------------------

    #[test]
    fn entropy_of_uniform_string_is_zero() {
        assert_eq!(shannon_entropy(&"a".repeat(40)), 0.0);
    }

    #[test]
    fn entropy_of_four_equal_symbols_is_two_bits() {
        // Vier gleich häufige Symbole ⇒ genau log2(4) = 2 bit/Zeichen.
        assert!((shannon_entropy("abcd") - 2.0).abs() < 1e-9);
    }

    #[test]
    fn entropy_of_empty_string_is_zero() {
        assert_eq!(shannon_entropy(""), 0.0);
    }

    #[test]
    fn detects_high_entropy_base64_blob() {
        // 44 Zeichen zufälliges base64 — deutlich über der Schwelle.
        let blob = "n8Kf3pQx7ZrLm2WvB9tAeYcJd4Ghs6UoTiRlPnQxZ0a";
        assert!(blob.len() >= DEFAULT_MIN_LEN);
        let (text, n) = redact_with(HighEntropyRedactor::new(), &format!("secret={blob}"));
        assert_eq!(text, "secret=[redacted:secret]");
        assert_eq!(n, 1);
    }

    #[test]
    fn low_entropy_prose_is_not_flagged() {
        let prose = "the quick brown fox jumps over the lazy dog again";
        let (text, n) = redact_with(HighEntropyRedactor::new(), prose);
        assert_eq!(text, prose);
        assert_eq!(n, 0);
    }

    #[test]
    fn short_high_entropy_run_is_below_min_len() {
        // Hohe Entropie, aber kürzer als DEFAULT_MIN_LEN ⇒ kein Fund.
        let (text, n) = redact_with(HighEntropyRedactor::new(), "aB3xZ9qL");
        assert_eq!(text, "aB3xZ9qL");
        assert_eq!(n, 0);
    }

    #[test]
    fn hex_sha1_is_not_flagged_by_default() {
        // 40 Hex-Zeichen (Git-SHA-Form): max. Entropie 4.0 bit < 4.5 ⇒ kein
        // Fehlalarm. Genau die Sorte False-Positive, die wir vermeiden wollen.
        let sha = "356a192b7913b04c54574d18c28d46e6395428ab";
        let (text, n) = redact_with(HighEntropyRedactor::new(), sha);
        assert_eq!(text, sha);
        assert_eq!(n, 0);
    }

    #[test]
    fn long_slash_path_is_not_flagged() {
        // Ein langer, ununterbrochener Pfad-Lauf (`/` ist im Alphabet) — 44
        // Zeichen, aber Entropie ~3.8 < 4.5. Kein Fehlalarm.
        let path = "/usr/local/share/applications/mimeinfo/cache";
        let (text, n) = redact_with(HighEntropyRedactor::new(), path);
        assert_eq!(text, path);
        assert_eq!(n, 0);
    }

    #[test]
    fn high_entropy_multibyte_context_is_preserved() {
        let blob = "n8Kf3pQx7ZrLm2WvB9tAeYcJd4Ghs6UoTiRlPnQxZ0a";
        let (text, n) = redact_with(HighEntropyRedactor::new(), &format!("🦀 {blob} café"));
        assert_eq!(text, "🦀 [redacted:secret] café");
        assert_eq!(n, 1);
    }

    #[test]
    fn threshold_is_configurable() {
        // Mit sehr niedriger Schwelle wird auch ein längeres Wort erfasst …
        let word = "constantatetetetetetetetetetuvwxyz";
        assert!(word.len() >= 20);
        let (_, hit) = redact_with(HighEntropyRedactor::with_params(20, 1.0), word);
        assert_eq!(hit, 1);
        // … mit der Default-Schwelle nicht.
        let (_, miss) = redact_with(HighEntropyRedactor::new(), word);
        assert_eq!(miss, 0);
    }

    #[test]
    fn high_entropy_name_is_stable() {
        assert_eq!(HighEntropyRedactor::new().name(), "high-entropy");
    }

    // --- Zusammenspiel beider Detektoren --------------------------------------

    #[test]
    fn both_detectors_on_one_secret_merge_to_single_finding() {
        // Ein zufälliges GitHub-Token ist zugleich hoch-entropisch (H≈4.87 auf
        // 40 Zeichen): known-token UND high-entropy feuern auf denselben Span.
        // Die Pipeline führt die Überlappung zusammen — ein Platzhalter, ein
        // Zähler (Dedup gratis).
        let token = "ghp_w2wMqZcUDIh7yfJs1ON43xKmTecQoXsf2o3g";
        let out = RedactionPipeline::new()
            .with(KnownTokenRedactor::new())
            .with(HighEntropyRedactor::new())
            .redact(&format!("token={token}"));
        assert_eq!(out.text, "token=[redacted:secret]");
        assert_eq!(out.counts.secrets, 1);
    }

    #[test]
    fn realistic_config_snippet_redacts_only_the_secret() {
        // Prosa/Struktur bleibt, nur der Key verschwindet.
        let snippet =
            "region = eu-central-1\naws_access_key_id = AKIAIOSFODNN7EXAMPLE\nenabled = true";
        let out = RedactionPipeline::new()
            .with(KnownTokenRedactor::new())
            .with(HighEntropyRedactor::new())
            .redact(snippet);
        assert!(out.text.contains("region = eu-central-1"));
        assert!(out.text.contains("enabled = true"));
        assert!(!out.text.contains("AKIAIOSFODNN7EXAMPLE"));
        assert_eq!(out.counts.secrets, 1);
    }
}
