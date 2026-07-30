//! The `corral-todo` CLI: the only sanctioned writer of `todo.txt`.
//!
//! Argument parsing is hand-rolled into a pure `Command::parse` so the grammar
//! is unit-tested without spawning a process, and so the workspace gains no
//! argument-parsing dependency for four subcommands.

use corral_core::launch::TerminalLauncher;
use corral_todo::item::{Item, State};
use corral_todo::state::{apply, Change};
use corral_todo::store::Store;
use corral_todo::wake::POLICY_FILE;
use corral_todo::watch::Watcher;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The dispatcher policy shipped with the binary, written into a todo directory
/// by `init`. Embedded from the repo's own `DISPATCHER.md`, so the default can
/// never drift from the documented one, and so a live todo directory needs no
/// symlink back into a git checkout (where a `git pull` would silently change
/// how the dispatcher behaves).
const POLICY_TEMPLATE: &str = include_str!("../DISPATCHER.md");

#[derive(Debug, PartialEq, Eq)]
enum Command {
    List {
        filter: Option<State>,
    },
    Add {
        text: String,
    },
    Set {
        id: String,
        change: Change,
    },
    Archive,
    Watch {
        dir: Option<String>,
        interval: u64,
        dispatch_argv: Vec<String>,
    },
    Init {
        dir: String,
        force: bool,
    },
}

const USAGE: &str = "\
corral-todo list [--open|--status <open|progress|blocked|done>]
corral-todo add \"<text>\"
corral-todo set <id> <open|progress|blocked|done> [--target <dir>] [--worker <session>] [--reason <text>]
corral-todo archive
corral-todo watch [--dir <dir>] [--interval <secs>] -- <dispatcher argv...>
corral-todo init <dir> [--force]
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
            "watch" => {
                let dir = take_flag(&mut rest, "--dir")?;
                let interval = take_flag(&mut rest, "--interval")?
                    .map(|s| s.parse::<u64>().map_err(|e| format!("--interval: {e}")))
                    .transpose()?
                    .unwrap_or(5);
                let at = rest.iter().position(|a| a == "--");
                let dispatch_argv = match at {
                    Some(at) => rest.split_off(at + 1),
                    None => Vec::new(),
                };
                if dispatch_argv.is_empty() {
                    // Never default the harness: corral's own rule is that it
                    // does not name an agent kind, and neither does this.
                    return Err(
                        "watch needs a dispatcher argv after --, e.g. `watch --dir ~/todos -- pi`"
                            .into(),
                    );
                }
                // Drop the `--` separator itself before the flag check.
                rest.retain(|a| a != "--");
                reject_unknown_flags(&rest)?;
                Ok(Command::Watch {
                    dir,
                    interval,
                    dispatch_argv,
                })
            }
            "init" => {
                let force = rest.iter().any(|a| a == "--force");
                rest.retain(|a| a != "--force");
                reject_unknown_flags(&rest)?;
                let dir = rest.first().ok_or("init needs a directory")?.clone();
                Ok(Command::Init { dir, force })
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
        Command::Watch {
            dir,
            interval,
            dispatch_argv,
        } => {
            // `--dir` names a todo directory, whose file is `<dir>/todo.txt`.
            // Without it, watch the file the store already resolved (`--file`,
            // else $CORRAL_TODO_FILE, else ./todo.txt) and take its parent as
            // the directory. Either way the watched file and the directory
            // agree by construction, so they cannot name different places.
            let (file, dir) = match dir {
                Some(d) => (PathBuf::from(&d).join("todo.txt"), PathBuf::from(d)),
                None => (
                    store.path().to_path_buf(),
                    store
                        .path()
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_default(),
                ),
            };
            let mut watcher = Watcher::new(
                Store::new(file),
                dir,
                dispatch_argv,
                Duration::from_secs(interval),
            );
            watcher.run(&TerminalLauncher);
            Ok(())
        }
        Command::Init { dir, force } => init(Path::new(&dir), force),
    }
}

/// Set up a todo directory: the directory itself, an empty `todo.txt`, and the
/// dispatcher policy. Prints the whitelist lines rather than writing them:
/// `~/.corral/whitelist` grants cross-directory authorization and stays
/// operator-owned, so a todo tool must not edit it (SECURITY.md).
fn init(dir: &Path, force: bool) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", dir.display()))?;

    let policy = dir.join(POLICY_FILE);
    if policy.exists() && !force {
        // The file becomes the operator's once tuned, so overwriting it needs
        // an explicit ask.
        return Err(format!(
            "{} already exists; pass --force to replace it",
            policy.display()
        ));
    }
    std::fs::write(&policy, POLICY_TEMPLATE)
        .map_err(|e| format!("cannot write {}: {e}", policy.display()))?;

    let todo = dir.join("todo.txt");
    let created_todo = !todo.exists();
    if created_todo {
        std::fs::write(&todo, "").map_err(|e| format!("cannot write {}: {e}", todo.display()))?;
    }

    let shown = dir.display();
    println!("{:<8} {}", "wrote", policy.display());
    println!(
        "{:<8} {}",
        if created_todo { "created" } else { "kept" },
        todo.display()
    );
    println!();
    println!("Next, for each directory workers may run in, add both lines to");
    println!("~/.corral/whitelist (authorization is directional: one for the");
    println!("spawn, one for the handshake and the report):");
    println!();
    println!("    {shown} -> /path/to/worker/dir");
    println!("    /path/to/worker/dir -> {shown}");
    println!();
    println!("Then start the watcher, naming your harness:");
    println!();
    println!("    corral-todo watch --dir {shown} -- pi");
    Ok(())
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

    #[test]
    fn parses_watch_with_its_dispatch_argv() {
        assert_eq!(
            parse(&[
                "watch",
                "--dir",
                "/home/me/todos",
                "--interval",
                "2",
                "--",
                "pi"
            ])
            .unwrap(),
            Command::Watch {
                dir: Some("/home/me/todos".into()),
                interval: 2,
                dispatch_argv: vec!["pi".into()],
            }
        );
    }

    #[test]
    fn watch_defaults_its_interval() {
        let Command::Watch { interval, .. } = parse(&["watch", "--", "pi"]).unwrap() else {
            panic!("expected watch");
        };
        assert_eq!(interval, 5);
    }

    #[test]
    fn watch_keeps_a_multi_word_dispatcher_argv() {
        let Command::Watch { dispatch_argv, .. } =
            parse(&["watch", "--", "pi", "--model", "x"]).unwrap()
        else {
            panic!("expected watch");
        };
        // Flags after `--` belong to the harness, not to corral-todo.
        assert_eq!(dispatch_argv, vec!["pi", "--model", "x"]);
    }

    #[test]
    fn watch_refuses_to_default_the_harness() {
        assert!(parse(&["watch", "--dir", "/home/me/todos"]).is_err());
    }

    #[test]
    fn parses_init() {
        assert_eq!(
            parse(&["init", "/home/me/todos"]).unwrap(),
            Command::Init {
                dir: "/home/me/todos".into(),
                force: false
            }
        );
        assert_eq!(
            parse(&["init", "/home/me/todos", "--force"]).unwrap(),
            Command::Init {
                dir: "/home/me/todos".into(),
                force: true
            }
        );
        assert!(parse(&["init"]).is_err());
    }

    #[test]
    fn init_writes_the_policy_and_an_empty_todo_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("todos");
        init(&target, false).unwrap();
        let policy = std::fs::read_to_string(target.join(POLICY_FILE)).unwrap();
        // The embedded copy is the repo's own DISPATCHER.md, verbatim.
        assert_eq!(policy, POLICY_TEMPLATE);
        assert!(policy.contains("dispatcher"));
        assert_eq!(
            std::fs::read_to_string(target.join("todo.txt")).unwrap(),
            ""
        );
        // Never the ambient name: that would govern unrelated sessions here.
        assert!(!target.join("AGENTS.md").exists());
    }

    #[test]
    fn init_refuses_to_clobber_a_tuned_policy_without_force() {
        let dir = tempfile::tempdir().unwrap();
        init(dir.path(), false).unwrap();
        std::fs::write(dir.path().join(POLICY_FILE), "my own rules").unwrap();
        let err = init(dir.path(), false).unwrap_err();
        assert!(err.contains("--force"), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap(),
            "my own rules"
        );
        init(dir.path(), true).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(POLICY_FILE)).unwrap(),
            POLICY_TEMPLATE
        );
    }

    #[test]
    fn init_keeps_an_existing_todo_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("todo.txt"), "an existing idea\n").unwrap();
        init(dir.path(), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("todo.txt")).unwrap(),
            "an existing idea\n"
        );
    }
}
