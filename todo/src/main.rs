//! The `corral-todo` CLI: the only sanctioned writer of `todo.txt`.
//!
//! Argument parsing is hand-rolled into a pure `Command::parse` so the grammar
//! is unit-tested without spawning a process, and so the workspace gains no
//! argument-parsing dependency for four subcommands.

use corral_todo::item::{Item, State};
use corral_todo::state::{apply, Change};
use corral_todo::store::Store;
use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
enum Command {
    List { filter: Option<State> },
    Add { text: String },
    Set { id: String, change: Change },
    Archive,
}

const USAGE: &str = "\
corral-todo list [--open|--status <open|progress|blocked|done>]
corral-todo add \"<text>\"
corral-todo set <id> <open|progress|blocked|done> [--target <dir>] [--worker <session>] [--reason <text>]
corral-todo archive
common: [--file <todo.txt>]  (else $CORRAL_TODO_FILE, else ./todo.txt)";

fn parse_state(word: &str) -> Result<State, String> {
    match word {
        "open" => Ok(State::Open),
        "progress" => Ok(State::Progress),
        "blocked" => Ok(State::Blocked),
        "done" => Ok(State::Done),
        other => Err(format!(
            "unknown state {other:?}; expected open|progress|blocked|done"
        )),
    }
}

/// Pull `--name <value>` out of the argument list, leaving the positionals.
fn take_flag(args: &mut Vec<String>, name: &str) -> Result<Option<String>, String> {
    let Some(at) = args.iter().position(|a| a == name) else {
        return Ok(None);
    };
    if at + 1 >= args.len() {
        return Err(format!("{name} needs a value"));
    }
    args.remove(at);
    Ok(Some(args.remove(at)))
}

impl Command {
    fn parse(args: &[String]) -> Result<Command, String> {
        let Some((verb, tail)) = args.split_first() else {
            return Err(format!("no subcommand\n{USAGE}"));
        };
        let mut rest = tail.to_vec();
        match verb.as_str() {
            "list" => {
                let status = take_flag(&mut rest, "--status")?;
                let open = rest.iter().any(|a| a == "--open");
                rest.retain(|a| a != "--open");
                reject_unknown_flags(&rest)?;
                let filter = match (open, status) {
                    (true, Some(_)) => return Err("--open and --status are exclusive".into()),
                    (true, None) => Some(State::Open),
                    (false, Some(s)) => Some(parse_state(&s)?),
                    (false, None) => None,
                };
                Ok(Command::List { filter })
            }
            "add" => {
                reject_unknown_flags(&rest)?;
                if rest.is_empty() {
                    return Err("add needs task text".into());
                }
                Ok(Command::Add {
                    text: rest.join(" "),
                })
            }
            "set" => {
                let target = take_flag(&mut rest, "--target")?;
                let worker = take_flag(&mut rest, "--worker")?;
                let reason = take_flag(&mut rest, "--reason")?;
                reject_unknown_flags(&rest)?;
                let id = rest.first().ok_or("set needs an id")?.clone();
                let state = parse_state(rest.get(1).ok_or("set needs a state")?)?;
                Ok(Command::Set {
                    id,
                    change: Change {
                        state,
                        target,
                        worker,
                        reason,
                    },
                })
            }
            "archive" => {
                reject_unknown_flags(&rest)?;
                Ok(Command::Archive)
            }
            other => Err(format!("unknown subcommand {other:?}\n{USAGE}")),
        }
    }
}

fn reject_unknown_flags(args: &[String]) -> Result<(), String> {
    // Fail loud: a typo'd flag silently treated as task text would write
    // nonsense into the file.
    if let Some(bad) = args.iter().find(|a| a.starts_with("--")) {
        return Err(format!("unknown option {bad}\n{USAGE}"));
    }
    Ok(())
}

fn todo_path(explicit: Option<String>) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CORRAL_TODO_FILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("todo.txt"))
}

/// One line per item: id, state, target, worker, then the text.
fn format_item(item: &Item) -> String {
    let state = match item.state() {
        State::Open => "open",
        State::Progress => "progress",
        State::Blocked => "blocked",
        State::Done => "done",
    };
    let mut out = format!("{:<4} {:<9}", item.key("id").unwrap_or("?"), state);
    if let Some(t) = item.key("target") {
        out.push_str(&format!(" target:{t}"));
    }
    if let Some(w) = item.key("worker") {
        out.push_str(&format!(" worker:{w}"));
    }
    let prose: Vec<&str> = item
        .rest
        .split_whitespace()
        .filter(|t| !t.contains(':'))
        .collect();
    out.push_str(&format!("  {}", prose.join(" ")));
    out
}

fn run(command: Command, store: &Store) -> Result<(), String> {
    match command {
        Command::List { filter } => {
            for item in store.read_normalized()? {
                if filter.is_none_or(|f| item.state() == f) {
                    println!("{}", format_item(&item));
                }
            }
            Ok(())
        }
        Command::Add { text } => {
            let id = store.mutate(|items| {
                let item = Item::parse(&text).ok_or_else(|| "task text is empty".to_string())?;
                items.push(item);
                // `mutate` normalized *before* this closure ran, so the item
                // just pushed has no id yet. Normalize again to coin one and be
                // able to print it. (Idempotent, so the existing items are
                // untouched.)
                let today = Store::today();
                corral_todo::normalize::normalize(items, &today);
                items
                    .last()
                    .and_then(|i| i.key("id"))
                    .map(|s| s.to_string())
                    .ok_or_else(|| "internal: added item got no id".to_string())
            })?;
            println!("{id}");
            Ok(())
        }
        Command::Set { id, change } => store.mutate(|items| {
            let item = items
                .iter_mut()
                .find(|i| i.key("id") == Some(id.as_str()))
                // Fail loud: a set against an id that is not there means the
                // caller's picture of the file is wrong.
                .ok_or_else(|| format!("no item with id {id}"))?;
            apply(item, &change, &Store::today())
        }),
        Command::Archive => {
            let moved = store.archive()?;
            println!("archived {moved}");
            Ok(())
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--file` is pulled only from before a literal `--`, so a dispatcher argv
    // (`watch -- pi --file x`) keeps its own flags instead of losing them here.
    let split = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let (mut head, tail) = (args[..split].to_vec(), args[split..].to_vec());
    let file = match take_flag(&mut head, "--file") {
        Ok(f) => f,
        Err(e) => {
            eprintln!("corral-todo: {e}");
            std::process::exit(2);
        }
    };
    head.extend(tail);
    let args = head;
    let command = match Command::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("corral-todo: {e}");
            std::process::exit(2);
        }
    };
    if let Err(e) = run(command, &Store::new(todo_path(file))) {
        eprintln!("corral-todo: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, String> {
        Command::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn parses_list_filters() {
        assert_eq!(parse(&["list"]).unwrap(), Command::List { filter: None });
        assert_eq!(
            parse(&["list", "--open"]).unwrap(),
            Command::List {
                filter: Some(State::Open)
            }
        );
        assert_eq!(
            parse(&["list", "--status", "progress"]).unwrap(),
            Command::List {
                filter: Some(State::Progress)
            }
        );
    }

    #[test]
    fn parses_add_joining_its_words() {
        assert_eq!(
            parse(&["add", "do", "a", "thing"]).unwrap(),
            Command::Add {
                text: "do a thing".into()
            }
        );
    }

    #[test]
    fn parses_set_with_its_options() {
        assert_eq!(
            parse(&["set", "a7f", "progress", "--target", "/srv/x", "--worker", "W1"]).unwrap(),
            Command::Set {
                id: "a7f".into(),
                change: Change {
                    state: State::Progress,
                    target: Some("/srv/x".into()),
                    worker: Some("W1".into()),
                    reason: None
                },
            }
        );
    }

    #[test]
    fn rejects_an_unknown_state_and_an_unknown_flag() {
        assert!(parse(&["set", "a7f", "wat"]).is_err());
        assert!(parse(&["list", "--wat"]).is_err());
        assert!(parse(&["wat"]).is_err());
    }

    #[test]
    fn rejects_a_flag_with_no_value() {
        assert!(parse(&["set", "a7f", "blocked", "--reason"]).is_err());
    }

    #[test]
    fn requires_a_subcommand() {
        assert!(parse(&[]).is_err());
    }
}
