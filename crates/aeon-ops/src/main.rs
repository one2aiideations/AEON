//! The `aeon-ops` command-line program: set up a database, load demo data, run reports, run the
//! self-test and serve the HTTP API.
#![forbid(unsafe_code)]

use aeon_ops::common::{self, Clock, Ctx, SystemClock};
use aeon_ops::{admin_reports, demo, schema, selftest, server};
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::Mutex;

const DEFAULT_DB: &str = "aeon_ops.db";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Init {
        db: String,
    },
    Bootstrap {
        tenant: String,
        admin_user: String,
        db: String,
    },
    Demo {
        tenant: String,
        db: String,
    },
    Test,
    Report {
        name: String,
        tenant: String,
        user: String,
        db: String,
        args: serde_json::Value,
    },
    Serve {
        bind: String,
        db: String,
    },
    Version,
}

fn usage() -> String {
    "usage: aeon-ops <command> [options]\n  init [--db FILE]\n  bootstrap <tenant> <admin_user> [--db FILE]\n  demo <tenant> [--db FILE]\n  test\n  report <name> <tenant> <user> [--db FILE] [key=value ...]\n  serve [--bind ADDR:PORT] [--db FILE]\n  version".to_string()
}

type Flags = HashMap<String, String>;

/// Splits arguments into positional ones and `--name value` flags (only `allowed` names).
fn split_flags(args: &[String], allowed: &[&str]) -> Result<(Vec<String>, Flags), String> {
    let mut positional = Vec::new();
    let mut flags = HashMap::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(name) = arg.strip_prefix("--") {
            if !allowed.contains(&name) {
                return Err(format!("unknown option --{name}"));
            }
            let value = iter
                .next()
                .ok_or_else(|| format!("missing value for --{name}"))?;
            flags.insert(name.to_string(), value.clone());
        } else {
            positional.push(arg.clone());
        }
    }
    Ok((positional, flags))
}

fn flag_db(flags: &Flags) -> String {
    flags
        .get("db")
        .cloned()
        .unwrap_or_else(|| DEFAULT_DB.to_string())
}

fn parse_args(args: &[String]) -> Result<Command, String> {
    let (cmd, rest) = args.split_first().ok_or_else(usage)?;
    match cmd.as_str() {
        "init" => {
            let (pos, fl) = split_flags(rest, &["db"])?;
            if !pos.is_empty() {
                return Err(usage());
            }
            Ok(Command::Init { db: flag_db(&fl) })
        }
        "bootstrap" => {
            let (pos, fl) = split_flags(rest, &["db"])?;
            let [tenant, admin_user] = pos.as_slice() else {
                return Err(usage());
            };
            Ok(Command::Bootstrap {
                tenant: tenant.clone(),
                admin_user: admin_user.clone(),
                db: flag_db(&fl),
            })
        }
        "demo" => {
            let (pos, fl) = split_flags(rest, &["db"])?;
            let [tenant] = pos.as_slice() else {
                return Err(usage());
            };
            Ok(Command::Demo {
                tenant: tenant.clone(),
                db: flag_db(&fl),
            })
        }
        "test" => {
            if !rest.is_empty() {
                return Err(usage());
            }
            Ok(Command::Test)
        }
        "report" => {
            let (pos, fl) = split_flags(rest, &["db"])?;
            let (Some(name), Some(tenant), Some(user)) = (pos.first(), pos.get(1), pos.get(2))
            else {
                return Err(usage());
            };
            let mut map = serde_json::Map::new();
            for kv in pos.iter().skip(3) {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| format!("expected key=value, got '{kv}'"))?;
                if k.is_empty() {
                    return Err(format!("empty key in '{kv}'"));
                }
                let value = if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
                    v.parse::<i64>()
                        .map_or_else(|_| serde_json::Value::from(v), serde_json::Value::from)
                } else {
                    serde_json::Value::from(v)
                };
                map.insert(k.to_string(), value);
            }
            Ok(Command::Report {
                name: name.clone(),
                tenant: tenant.clone(),
                user: user.clone(),
                db: flag_db(&fl),
                args: serde_json::Value::Object(map),
            })
        }
        "serve" => {
            let (pos, fl) = split_flags(rest, &["bind", "db"])?;
            if !pos.is_empty() {
                return Err(usage());
            }
            Ok(Command::Serve {
                bind: fl
                    .get("bind")
                    .cloned()
                    .unwrap_or_else(|| "127.0.0.1:8787".to_string()),
                db: flag_db(&fl),
            })
        }
        "version" => {
            if !rest.is_empty() {
                return Err(usage());
            }
            Ok(Command::Version)
        }
        _ => Err(usage()),
    }
}

/// Opens the database and makes sure every table exists (safe to repeat).
fn open_ready(db: &str) -> Result<Connection, String> {
    let conn = common::open_db(db)?;
    schema::create_schema(&conn)?;
    Ok(conn)
}

fn print_json(value: &serde_json::Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(common::e)?
    );
    Ok(())
}

fn execute(cmd: Command) -> Result<(), String> {
    match cmd {
        Command::Init { db } => {
            open_ready(&db)?;
            println!("initialized schema in {db}");
            Ok(())
        }
        Command::Bootstrap {
            tenant,
            admin_user,
            db,
        } => {
            let conn = open_ready(&db)?;
            admin_reports::bootstrap_tenant(&conn, &tenant, &admin_user, SystemClock.now())?;
            println!("bootstrapped tenant '{tenant}' with admin user '{admin_user}'");
            Ok(())
        }
        Command::Demo { tenant, db } => {
            let conn = open_ready(&db)?;
            let ledger = server::LoggingLedger {
                entries: Mutex::new(Vec::new()),
            };
            print_json(&demo::load_demo(&conn, &ledger, &SystemClock, &tenant)?)
        }
        Command::Test => print_json(&selftest::run_selftest()?),
        Command::Report {
            name,
            tenant,
            user,
            db,
            args,
        } => {
            let conn = open_ready(&db)?;
            let ctx = Ctx {
                tenant,
                user,
                roles: vec!["admin".to_string()],
                company: None,
            };
            print_json(&admin_reports::run_report(&conn, &ctx, &name, &args)?)
        }
        Command::Serve { bind, db } => {
            // Create the tables once; each request opens its own connection afterwards.
            open_ready(&db)?;
            server::serve(&server::ServerConfig {
                bind,
                db_path: db,
                max_body: 1_048_576,
                read_timeout_secs: 10,
            })
        }
        Command::Version => {
            println!("aeon-ops {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&argv) {
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::from(2)
        }
        Ok(cmd) => match execute(cmd) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("aeon-ops: {msg}");
                std::process::ExitCode::from(1)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn db(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn version_and_test_take_no_arguments() {
        assert_eq!(parse_args(&a(&["version"])), Ok(Command::Version));
        assert_eq!(parse_args(&a(&["test"])), Ok(Command::Test));
        assert!(parse_args(&a(&["test", "extra"])).is_err());
        assert!(parse_args(&a(&["version", "extra"])).is_err());
    }

    #[test]
    fn init_uses_the_default_or_a_given_database() {
        assert_eq!(
            parse_args(&a(&["init"])),
            Ok(Command::Init { db: db(DEFAULT_DB) })
        );
        assert_eq!(
            parse_args(&a(&["init", "--db", "x.db"])),
            Ok(Command::Init { db: db("x.db") })
        );
        assert!(parse_args(&a(&["init", "surprise"])).is_err());
    }

    #[test]
    fn bootstrap_needs_a_tenant_and_a_user() {
        assert_eq!(
            parse_args(&a(&["bootstrap", "acme", "root"])),
            Ok(Command::Bootstrap {
                tenant: db("acme"),
                admin_user: db("root"),
                db: db(DEFAULT_DB)
            })
        );
        assert_eq!(
            parse_args(&a(&["bootstrap", "acme", "root", "--db", "b.db"])),
            Ok(Command::Bootstrap {
                tenant: db("acme"),
                admin_user: db("root"),
                db: db("b.db")
            })
        );
        assert!(parse_args(&a(&["bootstrap", "acme"])).is_err());
        assert!(parse_args(&a(&["bootstrap", "a", "b", "c"])).is_err());
    }

    #[test]
    fn demo_needs_exactly_a_tenant() {
        assert_eq!(
            parse_args(&a(&["demo", "acme"])),
            Ok(Command::Demo {
                tenant: db("acme"),
                db: db(DEFAULT_DB)
            })
        );
        assert!(parse_args(&a(&["demo"])).is_err());
        assert!(parse_args(&a(&["demo", "a", "b"])).is_err());
    }

    #[test]
    fn serve_has_loopback_defaults() {
        assert_eq!(
            parse_args(&a(&["serve"])),
            Ok(Command::Serve {
                bind: db("127.0.0.1:8787"),
                db: db(DEFAULT_DB)
            })
        );
        assert_eq!(
            parse_args(&a(&["serve", "--bind", "0.0.0.0:9000", "--db", "s.db"])),
            Ok(Command::Serve {
                bind: db("0.0.0.0:9000"),
                db: db("s.db")
            })
        );
    }

    #[test]
    fn unknown_commands_options_and_missing_values_are_errors() {
        assert!(parse_args(&a(&["frobnicate"])).is_err());
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&a(&["init", "--db"])).is_err());
        assert!(parse_args(&a(&["init", "--bogus", "x"])).is_err());
        assert!(parse_args(&a(&["serve", "--db"])).is_err());
    }

    #[test]
    fn report_arguments_become_integers_only_when_all_digits() {
        let cmd = parse_args(&a(&[
            "report",
            "payroll",
            "acme",
            "root",
            "period=2026-03",
            "count=7",
            "zip=007x",
            "empty=",
        ]));
        let Ok(Command::Report {
            name,
            tenant,
            user,
            args,
            ..
        }) = cmd
        else {
            panic!("expected a report command");
        };
        assert_eq!(
            (name.as_str(), tenant.as_str(), user.as_str()),
            ("payroll", "acme", "root")
        );
        assert_eq!(args["period"], "2026-03");
        assert_eq!(args["count"], 7);
        assert_eq!(args["zip"], "007x");
        assert_eq!(args["empty"], "");
    }

    #[test]
    fn report_needs_name_tenant_user_and_well_formed_pairs() {
        assert!(parse_args(&a(&["report", "stock", "acme"])).is_err());
        assert!(parse_args(&a(&["report", "stock", "acme", "root", "noequals"])).is_err());
        assert!(parse_args(&a(&["report", "stock", "acme", "root", "=v"])).is_err());
    }

    #[test]
    fn huge_numbers_stay_text_instead_of_overflowing() {
        let cmd = parse_args(&a(&[
            "report",
            "stock",
            "t",
            "u",
            "n=99999999999999999999999",
        ]));
        let Ok(Command::Report { args, .. }) = cmd else {
            panic!("expected a report command");
        };
        assert_eq!(args["n"], "99999999999999999999999");
    }
}
