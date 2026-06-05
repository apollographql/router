mod chapters;

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

// ── ANSI helpers ─────────────────────────────────────────────────────────────

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";

fn bold(s: &str) -> String { format!("{BOLD}{s}{RESET}") }
fn dim(s: &str) -> String { format!("{DIM}{s}{RESET}") }
fn green(s: &str) -> String { format!("{GREEN}{s}{RESET}") }
fn red(s: &str) -> String { format!("{RED}{s}{RESET}") }
fn yellow(s: &str) -> String { format!("{YELLOW}{s}{RESET}") }
fn cyan(s: &str) -> String { format!("{CYAN}{s}{RESET}") }
fn blue(s: &str) -> String { format!("{BLUE}{s}{RESET}") }
fn magenta(s: &str) -> String { format!("{MAGENTA}{s}{RESET}") }

// ── Data types ────────────────────────────────────────────────────────────────

pub struct Chapter {
    pub number: usize,
    pub title: &'static str,
    pub tagline: &'static str,
    pub exercises: Vec<Exercise>,
}

pub struct Exercise {
    pub title: &'static str,
    /// The concept reading that precedes the questions. Markdown-ish plain text.
    pub reading: &'static str,
    pub questions: Vec<Question>,
    /// An optional note shown after all questions pass, aimed at engineers who
    /// want to go deeper into the source code.
    pub engineer_note: Option<&'static str>,
}

pub enum Question {
    /// Standard A/B/C/D multiple-choice question.
    MultipleChoice {
        stem: &'static str,
        options: [&'static str; 4],
        /// 0 = A, 1 = B, 2 = C, 3 = D
        answer: usize,
        explanation: &'static str,
    },
    /// Ask the learner to look something up in the actual codebase.
    CodeFind {
        prompt: &'static str,
        file_hint: &'static str,
        /// All accepted answers (case-insensitive, trimmed).
        accepted: &'static [&'static str],
        hint: &'static str,
        explanation: &'static str,
    },
    /// Open reflection — the learner reads, thinks, and presses Enter to
    /// continue.  No right/wrong answer is checked.
    Reflection {
        prompt: &'static str,
        key_points: &'static [&'static str],
    },
}

// ── Progress tracking ─────────────────────────────────────────────────────────

fn progress_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".progress")
}

fn load_progress() -> usize {
    fs::read_to_string(progress_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn save_progress(completed: usize) {
    let _ = fs::write(progress_path(), completed.to_string());
}

fn reset_progress() {
    let _ = fs::remove_file(progress_path());
    println!("{}", green("Progress reset. Starting from the beginning next run."));
}

// ── Display helpers ───────────────────────────────────────────────────────────

fn clear_screen() {
    print!("\x1b[2J\x1b[H");
    let _ = io::stdout().flush();
}

fn horizontal_rule() {
    println!("{}", dim(&"─".repeat(68)));
}

fn section_header(chapter: usize, title: &str, tagline: &str) {
    horizontal_rule();
    println!("  {}  ·  {}", bold(&format!("Chapter {chapter}")), cyan(title));
    println!("  {}", dim(tagline));
    horizontal_rule();
    println!();
}

fn exercise_header(chapter: usize, ex_num: usize, ex_total: usize, title: &str) {
    horizontal_rule();
    println!(
        "  {}  ·  Exercise {ex_num}/{ex_total}",
        bold(&format!("Chapter {chapter}"))
    );
    println!("  {}", bold(title));
    horizontal_rule();
    println!();
}

fn print_reading(text: &str) {
    for line in text.lines() {
        // Lines starting with "##" become bold headers
        if let Some(rest) = line.strip_prefix("## ") {
            println!("  {}", bold(rest));
        } else if let Some(rest) = line.strip_prefix("# ") {
            println!("  {}", bold(&rest.to_uppercase()));
        } else if line.trim_start().starts_with("- ") || line.trim_start().starts_with("• ") {
            println!("  {line}");
        } else if line.starts_with("    ") || line.starts_with('\t') {
            // code-like indented block
            println!("{}", cyan(line));
        } else if line.is_empty() {
            println!();
        } else {
            // Wrap at ~66 chars for readability
            print_wrapped(line, 66, "  ");
        }
    }
    println!();
}

fn print_wrapped(text: &str, width: usize, indent: &str) {
    let mut line = indent.to_string();
    for word in text.split_whitespace() {
        if line.len() + word.len() + 1 > width + indent.len() && line.len() > indent.len() {
            println!("{line}");
            line = format!("{indent}{word}");
        } else if line.len() == indent.len() {
            line.push_str(word);
        } else {
            line.push(' ');
            line.push_str(word);
        }
    }
    if line.len() > indent.len() {
        println!("{line}");
    }
}

fn press_enter(prompt: &str) {
    print!("  {} ", dim(prompt));
    let _ = io::stdout().flush();
    let mut buf = String::new();
    let _ = io::stdin().read_line(&mut buf);
}

fn ask(prompt: &str) -> String {
    print!("  {} ", prompt);
    let _ = io::stdout().flush();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf).unwrap_or(0);
    buf.trim().to_string()
}

fn progress_bar(done: usize, total: usize) -> String {
    let width = 36;
    let filled = (done * width) / total.max(1);
    let bar: String = (0..width)
        .map(|i| if i < filled { '█' } else { '░' })
        .collect();
    format!("  {CYAN}{bar}{RESET}  {done}/{total}")
}

fn welcome_screen(all_chapters: &[Chapter], completed: usize) {
    clear_screen();
    let total_exercises: usize = all_chapters.iter().map(|c| c.exercises.len()).sum();

    println!();
    println!(
        "  {}",
        bold("╔═════════════════════════════════════════════════════════════╗")
    );
    println!(
        "  {}",
        bold("║        Apollo Router Learnings                             ║")
    );
    println!(
        "  {}",
        bold("╚═════════════════════════════════════════════════════════════╝")
    );
    println!();
    println!("  A guided tour of Apollo Router for engineers, managers,");
    println!("  and everyone in between.");
    println!();
    println!(
        "  {} chapters  ·  {} exercises  ·  ~{}h {}min",
        all_chapters.len(),
        total_exercises,
        (total_exercises * 4) / 60,
        (total_exercises * 4) % 60
    );
    println!();
    if completed > 0 {
        println!("{}", progress_bar(completed, total_exercises));
        println!();
    }
    println!(
        "  {}",
        dim("Commands: 'list' to see chapters · 'reset' to start over · Ctrl+C to quit")
    );
    println!();
}

fn list_chapters(all_chapters: &[Chapter], completed_count: usize) {
    let mut running_total = 0;
    println!();
    for ch in all_chapters {
        let ch_done = (completed_count).saturating_sub(running_total).min(ch.exercises.len());
        let done_marker = if ch_done == ch.exercises.len() {
            green("✓")
        } else if ch_done > 0 {
            yellow("◑")
        } else {
            dim("○").to_string()
        };
        println!(
            "  {} {}  {}",
            done_marker,
            bold(&format!("Chapter {}:", ch.number)),
            ch.title
        );
        println!("       {}", dim(ch.tagline));
        println!("       {} exercises", ch.exercises.len());
        println!();
        running_total += ch.exercises.len();
    }
}

// ── Question runners ──────────────────────────────────────────────────────────

fn run_multiple_choice(
    stem: &str,
    options: &[&str; 4],
    answer: usize,
    explanation: &str,
) -> bool {
    println!("  {}", bold(stem));
    println!();
    for (i, opt) in options.iter().enumerate() {
        let letter = ["A", "B", "C", "D"][i];
        println!("    {}  {opt}", bold(letter));
    }
    println!();

    loop {
        let raw = ask(&format!("{}:", cyan("Your answer (A/B/C/D)")));
        let ans = raw.trim().to_uppercase();
        let idx = match ans.as_str() {
            "A" => Some(0usize),
            "B" => Some(1),
            "C" => Some(2),
            "D" => Some(3),
            "" => {
                println!("  {} (enter A, B, C, or D, or 's' to skip)", yellow("?"));
                continue;
            }
            "S" | "SKIP" => {
                println!();
                println!("  {} Skipped. The answer was {}.", yellow("→"), bold(["A","B","C","D"][answer]));
                println!();
                print_wrapped(explanation, 64, "    ");
                println!();
                return false;
            }
            _ => {
                println!("  {} (enter A, B, C, or D, or 's' to skip)", yellow("?"));
                continue;
            }
        };

        if idx == Some(answer) {
            println!();
            println!("  {} {}", green("✓"), bold("Correct!"));
            println!();
            print_wrapped(explanation, 64, "    ");
            println!();
            return true;
        } else {
            println!();
            println!("  {} Not quite — try again, or enter 's' to skip.", red("✗"));
            println!();
        }
    }
}

fn run_code_find(
    prompt: &str,
    file_hint: &str,
    accepted: &[&str],
    hint: &str,
    explanation: &str,
) -> bool {
    println!("  {}", bold("[ Code Exploration ]"));
    println!();
    print_wrapped(prompt, 64, "  ");
    println!();
    println!("  {}  {}", dim("Look in:"), cyan(file_hint));
    println!();

    let mut attempts = 0;
    loop {
        let raw = ask(&format!("{}:", cyan("Your answer")));
        let trimmed = raw.trim().to_lowercase();

        if trimmed == "s" || trimmed == "skip" {
            println!();
            println!("  {} Skipped.", yellow("→"));
            println!("    {}", dim(hint));
            println!();
            print_wrapped(explanation, 64, "    ");
            println!();
            return false;
        }

        let correct = accepted
            .iter()
            .any(|a| a.to_lowercase() == trimmed);

        if correct {
            println!();
            println!("  {} {}", green("✓"), bold("Correct!"));
            println!();
            print_wrapped(explanation, 64, "    ");
            println!();
            return true;
        }

        attempts += 1;
        if attempts == 2 {
            println!("  {} Not quite. Hint: {hint}", yellow("→"));
        } else if attempts >= 3 {
            println!("  {} Still not right — enter 's' to see the answer and move on.", red("→"));
        } else {
            println!("  {} Not quite — try again, or enter 's' to skip.", red("✗"));
        }
        println!();
    }
}

fn run_reflection(prompt: &str, key_points: &[&str]) {
    print_wrapped(prompt, 64, "  ");
    println!();
    if !key_points.is_empty() {
        println!("  {} Things to consider:", bold("→"));
        for point in key_points {
            println!("    • {point}");
        }
        println!();
    }
    press_enter("[Press Enter when you're ready to continue]");
}

// ── Core runner ───────────────────────────────────────────────────────────────

fn run_exercise(chapter: &Chapter, ex_num: usize, exercise: &Exercise) -> bool {
    clear_screen();
    exercise_header(chapter.number, ex_num, chapter.exercises.len(), exercise.title);
    print_reading(exercise.reading);

    if !exercise.questions.is_empty() {
        press_enter("[Press Enter to start the questions]");
    }

    let mut all_correct = true;
    for (q_idx, question) in exercise.questions.iter().enumerate() {
        clear_screen();
        exercise_header(chapter.number, ex_num, chapter.exercises.len(), exercise.title);
        println!(
            "  {}",
            dim(&format!("Question {} of {}", q_idx + 1, exercise.questions.len()))
        );
        println!();

        let result = match question {
            Question::MultipleChoice { stem, options, answer, explanation } => {
                run_multiple_choice(stem, options, *answer, explanation)
            }
            Question::CodeFind { prompt, file_hint, accepted, hint, explanation } => {
                run_code_find(prompt, file_hint, accepted, hint, explanation)
            }
            Question::Reflection { prompt, key_points } => {
                run_reflection(prompt, key_points);
                true
            }
        };
        if !result {
            all_correct = false;
        }

        if q_idx + 1 < exercise.questions.len() {
            press_enter("[Press Enter for the next question]");
        }
    }

    if let Some(note) = exercise.engineer_note {
        println!();
        horizontal_rule();
        println!("  {} {}", magenta("⚙"), bold("Engineer deep-dive"));
        horizontal_rule();
        print_reading(note);
    }

    all_correct
}

fn run_interactive(all_chapters: &[Chapter], start_from: usize) {
    let total_exercises: usize = all_chapters.iter().map(|c| c.exercises.len()).sum();

    // Walk every exercise in order
    let mut global_idx = 0;
    'outer: for chapter in all_chapters {
        for (ex_num, exercise) in chapter.exercises.iter().enumerate() {
            if global_idx < start_from {
                global_idx += 1;
                continue;
            }

            // Show chapter intro on the first exercise of each chapter
            if ex_num == 0 {
                clear_screen();
                section_header(chapter.number, chapter.title, chapter.tagline);
                press_enter("[Press Enter to begin this chapter]");
            }

            run_exercise(chapter, ex_num + 1, exercise);

            let completed = global_idx + 1;
            save_progress(completed);

            println!();
            println!("{}", progress_bar(completed, total_exercises));
            println!();

            if completed == total_exercises {
                break 'outer;
            }

            let raw = ask(&format!(
                "{} n=next  s=skip chapter  q=quit  list=chapters :",
                dim("►")
            ));
            match raw.trim().to_lowercase().as_str() {
                "q" | "quit" => {
                    println!();
                    println!("  {}  Progress saved. Run again to continue.", green("✓"));
                    println!();
                    return;
                }
                "s" | "skip" => {
                    // skip to end of current chapter
                    let remaining = chapter.exercises.len() - ex_num - 1;
                    global_idx += remaining + 1;
                    let skipped_completed = global_idx;
                    save_progress(skipped_completed);
                    continue 'outer;
                }
                "list" => {
                    clear_screen();
                    list_chapters(all_chapters, global_idx + 1);
                    press_enter("[Press Enter to continue]");
                }
                _ => {} // default: next
            }

            global_idx += 1;
        }
    }

    // Finished!
    clear_screen();
    println!();
    println!("  {} {} {}", green("★"), bold("You've completed all exercises!"), green("★"));
    println!();
    println!("  You now have a solid mental model of how Apollo Router works —");
    println!("  from request lifecycle to release process.");
    println!();
    println!("  A few good next steps:");
    println!("    • Read DEVELOPMENT.md to set up a local build");
    println!("    • Browse examples/ for plugin patterns");
    println!("    • Check dev-docs/ for architecture decision records");
    println!();
    println!("{}", progress_bar(total_exercises, total_exercises));
    println!();
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let all_chapters = chapters::all();

    let total_exercises: usize = all_chapters.iter().map(|c| c.exercises.len()).sum();
    let completed = load_progress().min(total_exercises);

    match args.get(1).map(String::as_str) {
        Some("reset") => {
            reset_progress();
        }
        Some("list") => {
            list_chapters(&all_chapters, completed);
        }
        Some("chapter") => {
            let n: usize = args
                .get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(1)
                .max(1)
                .min(all_chapters.len());
            // Calculate the global exercise index for the start of that chapter
            let start: usize = all_chapters[..n - 1].iter().map(|c| c.exercises.len()).sum();
            welcome_screen(&all_chapters, completed);
            run_interactive(&all_chapters, start);
        }
        _ => {
            welcome_screen(&all_chapters, completed);

            let start = if completed > 0 && completed < total_exercises {
                let raw = ask(&format!(
                    "  {}  continue where you left off? (Y/n) :",
                    blue("►")
                ));
                if raw.trim().eq_ignore_ascii_case("n") {
                    0
                } else {
                    completed
                }
            } else if completed == total_exercises {
                let raw = ask(&format!(
                    "  {}  you've finished everything! restart from the beginning? (y/N) :",
                    green("★")
                ));
                if raw.trim().eq_ignore_ascii_case("y") {
                    reset_progress();
                    0
                } else {
                    return;
                }
            } else {
                let _ = ask(&format!("  {} Press Enter to begin :", blue("►")));
                0
            };

            run_interactive(&all_chapters, start);
        }
    }
}
