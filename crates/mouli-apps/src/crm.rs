//! The CRM application as capabilities.
//!
//! The CRM keeps its own rules (duplicate leads, one-step stage moves, an approval for large
//! deals) and its own audit chain. This adapter does not add a second permission system: the
//! workspace has already decided who may call what, so the CRM is called with full permissions
//! under two fixed names, one that asks and one that decides. The CRM's own rule that the
//! requester cannot approve then still holds, and the real four-eyes check is the workspace's:
//! `crm.approval.decide` is a consequential capability, so a second person must approve it there.

use mouli_crm::leads::LeadInput;
use mouli_crm::model::{Money, Stage};
use mouli_crm::{Config, Crm, Ctx, Error as CrmError, Outcome};
use mouli_interface::{Application, Call, CapabilitySpec, Error, Output, Result, Risk};
use std::cell::RefCell;
use std::rc::Rc;

/// The CRM, shared between the adapter and the screens.
pub type SharedCrm = Rc<RefCell<Crm>>;

/// Source of the current time in unix seconds.
pub type Clock = Box<dyn Fn() -> u64>;

const ASKER: &str = "workspace";
const DECIDER: &str = "workspace-approver";

/// The CRM application.
pub struct CrmApp {
    crm: SharedCrm,
    tenant: String,
    clock: Clock,
}

impl CrmApp {
    /// An application over `crm` for one `tenant`. `clock` supplies the time for the CRM's
    /// records and audit; pass a fixed one in tests.
    pub fn new(crm: SharedCrm, tenant: &str, clock: Clock) -> Self {
        Self {
            crm,
            tenant: tenant.into(),
            clock,
        }
    }

    /// A new, empty CRM with the default rules.
    pub fn new_crm() -> SharedCrm {
        Rc::new(RefCell::new(Crm::new(Config::default())))
    }

    fn ctx(&self, user: &str) -> Ctx {
        Ctx::new(&self.tenant, user, &["*"], (self.clock)())
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Handler(msg.into())
}

fn fail(e: CrmError) -> Error {
    bad(e.to_string())
}

fn arg<'a>(call: &'a Call, name: &str) -> Result<&'a str> {
    call.args
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| bad(format!("{name} is required")))
}

fn opt<'a>(call: &'a Call, name: &str) -> Option<&'a str> {
    call.args
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.trim().is_empty())
}

fn stage_of(s: &str) -> Result<Stage> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "prospecting" => Stage::Prospecting,
        "qualified" => Stage::Qualified,
        "proposal" => Stage::Proposal,
        "negotiation" => Stage::Negotiation,
        "won" | "closedwon" => Stage::ClosedWon,
        "lost" | "closedlost" => Stage::ClosedLost,
        other => {
            return Err(bad(format!(
                "unknown stage '{other}' (prospecting, qualified, proposal, negotiation, won, lost)"
            )))
        }
    })
}

/// Parses an amount such as `1250`, `1250.5` or `1250.50` into minor units, exactly (no floats).
fn minor_units(s: &str) -> Result<i64> {
    let s = s.trim();
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    let ok = !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && frac.len() <= 2
        && frac.bytes().all(|b| b.is_ascii_digit());
    if !ok {
        return Err(bad(format!(
            "amount '{s}' is not a number with at most two decimals"
        )));
    }
    let frac = format!("{frac:0<2}");
    let major: i64 = whole.parse().map_err(|_| bad("amount is too large"))?;
    major
        .checked_mul(100)
        .and_then(|m| m.checked_add(frac.parse::<i64>().unwrap_or(0)))
        .ok_or_else(|| bad("amount is too large"))
}

fn money(minor: i64, cur: &str) -> String {
    format!("{}.{:02} {cur}", minor / 100, minor % 100)
}

impl Application for CrmApp {
    fn name(&self) -> &str {
        "crm"
    }

    fn capabilities(&self) -> Vec<CapabilitySpec> {
        vec![
            CapabilitySpec::new("crm.lead.list", "list the leads", Risk::Read),
            CapabilitySpec::new(
                "crm.lead.capture",
                "add a lead (name=.. email=.. source=web [phone=..] [company=..] [key=..])",
                Risk::Write,
            )
            .requires(&["name", "email", "source"]),
            CapabilitySpec::new(
                "crm.lead.qualify",
                "mark a new lead qualified, or disqualified with reason=.. (id=lead-1)",
                Risk::Write,
            )
            .requires(&["id"]),
            CapabilitySpec::new(
                "crm.lead.convert",
                "turn a qualified lead into account, contact and deal (id=.. deal=.. amount=1000.00 [currency=EUR])",
                Risk::Write,
            )
            .requires(&["id", "deal", "amount"]),
            CapabilitySpec::new("crm.opp.list", "list the deals", Risk::Read),
            CapabilitySpec::new(
                "crm.opp.advance",
                "move a deal one stage on, or lose it with reason=.. (id=.. to=proposal)",
                Risk::Write,
            )
            .requires(&["id", "to"]),
            CapabilitySpec::new(
                "crm.approval.decide",
                "approve or refuse a held large deal (approval=appr-1 decision=approve|refuse); needs approval",
                Risk::Consequential,
            )
            .requires(&["approval", "decision"]),
            CapabilitySpec::new("crm.forecast", "the weighted pipeline forecast", Risk::Read),
        ]
    }

    fn invoke(&mut self, call: &Call) -> Result<Output> {
        let ctx = self.ctx(ASKER);
        let mut crm = self.crm.borrow_mut();
        let text = match call.capability.as_str() {
            "crm.lead.list" => {
                let rows: Vec<String> = crm
                    .leads_of(&ctx)
                    .iter()
                    .map(|l| {
                        format!(
                            "{}  {}  <{}>  {:?}  score {}",
                            l.id,
                            l.name,
                            l.email,
                            l.status,
                            l.score.as_ref().map_or(0, |s| s.points)
                        )
                    })
                    .collect();
                if rows.is_empty() {
                    "No leads.".to_string()
                } else {
                    rows.join("\n")
                }
            }
            "crm.lead.capture" => {
                let input = LeadInput {
                    name: arg(call, "name")?.into(),
                    email: arg(call, "email")?.into(),
                    phone: opt(call, "phone").map(Into::into),
                    company: opt(call, "company").map(Into::into),
                    source: arg(call, "source")?.into(),
                    consent: None,
                };
                let key = opt(call, "key").unwrap_or("");
                let id = crm.capture_lead(&ctx, input, key).map_err(fail)?;
                format!("Captured {id}.")
            }
            "crm.lead.qualify" => {
                let id = arg(call, "id")?;
                let reason = opt(call, "reason");
                crm.qualify_lead(&ctx, id, reason).map_err(fail)?;
                match reason {
                    Some(r) => format!("{id} disqualified: {r}."),
                    None => format!("{id} qualified."),
                }
            }
            "crm.lead.convert" => {
                let minor = minor_units(arg(call, "amount")?)?;
                let cur = opt(call, "currency").unwrap_or("EUR");
                let amount = Money::new(minor, cur).map_err(fail)?;
                let (a, c, o) = crm
                    .convert_lead(&ctx, arg(call, "id")?, arg(call, "deal")?, amount)
                    .map_err(fail)?;
                format!("Converted: account {a}, contact {c}, deal {o}.")
            }
            "crm.opp.list" => {
                let rows: Vec<String> = crm
                    .opportunities_of(&ctx)
                    .iter()
                    .map(|o| {
                        format!(
                            "{}  {}  {:?}  {}",
                            o.id,
                            o.name,
                            o.stage,
                            money(o.amount.minor, &o.amount.currency)
                        )
                    })
                    .collect();
                if rows.is_empty() {
                    "No deals.".to_string()
                } else {
                    rows.join("\n")
                }
            }
            "crm.opp.advance" => {
                let (id, to) = (arg(call, "id")?, stage_of(arg(call, "to")?)?);
                match crm
                    .advance(&ctx, id, to, opt(call, "reason"))
                    .map_err(fail)?
                {
                    Outcome::Done(s) => format!("{id} is now {s:?}."),
                    Outcome::Pending { approval } => format!(
                        "{id} is large enough to need approval. Held as {approval}: ask for \
                         crm.approval.decide approval={approval} decision=approve."
                    ),
                }
            }
            "crm.approval.decide" => {
                let approve = match arg(call, "decision")?.to_ascii_lowercase().as_str() {
                    "approve" => true,
                    "refuse" => false,
                    other => {
                        return Err(bad(format!("decision is approve or refuse, not '{other}'")))
                    }
                };
                let ctx = self.ctx(DECIDER);
                let id = arg(call, "approval")?;
                match crm.decide(&ctx, id, approve).map_err(fail)? {
                    Outcome::Done(s) if approve => format!("{id} approved; deal is {s:?}."),
                    Outcome::Done(_) => format!("{id} refused."),
                    Outcome::Pending { .. } => format!("{id} is still pending."),
                }
            }
            "crm.forecast" => {
                let f = crm.forecast(&ctx).map_err(fail)?;
                if f.is_empty() {
                    "No deals to forecast.".to_string()
                } else {
                    f.iter()
                        .map(|(cur, f)| {
                            format!(
                                "{cur}: open {}, weighted {}, won {}",
                                money(i64::try_from(f.open).unwrap_or(i64::MAX), cur),
                                money(i64::try_from(f.weighted).unwrap_or(i64::MAX), cur),
                                money(i64::try_from(f.won).unwrap_or(i64::MAX), cur),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            other => return Err(bad(format!("unknown capability {other}"))),
        };
        Ok(Output {
            text,
            artifacts: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mouli_interface::{Permissions, Policy, Reply, Workspace};
    use std::collections::BTreeMap;

    fn setup() -> (Workspace, SharedCrm) {
        let mut ws = Workspace::new(Policy::default(), Default::default()).unwrap();
        ws.add_role(Permissions::new("owner", &["*"]));
        ws.add_role(Permissions::new("sales", &["crm.*"]));
        ws.add_role(Permissions::new(
            "viewer",
            &["crm.lead.list", "crm.opp.list"],
        ));
        let crm = CrmApp::new_crm();
        ws.register_app(Box::new(CrmApp::new(
            crm.clone(),
            "acme",
            Box::new(|| 1_000),
        )))
        .unwrap();
        (ws, crm)
    }

    fn run(ws: &mut Workspace, sess: &str, cap: &str, args: &[(&str, &str)]) -> Result<Reply> {
        let a: BTreeMap<String, String> = args
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        ws.call("acme", sess, cap, a)
    }

    fn text(r: Reply) -> String {
        match r {
            Reply::Done(o) => o.text,
            other => panic!("{other:?}"),
        }
    }

    fn lead(ws: &mut Workspace, s: &str, email: &str) -> String {
        let t = text(
            run(
                ws,
                s,
                "crm.lead.capture",
                &[
                    ("name", "Ada"),
                    ("email", email),
                    ("source", "web"),
                    ("company", "Initech"),
                ],
            )
            .unwrap(),
        );
        t.trim_start_matches("Captured ")
            .trim_end_matches('.')
            .into()
    }

    #[test]
    fn a_lead_goes_from_capture_to_a_deal_through_the_gate() {
        let (mut ws, crm) = setup();
        let s = ws.open_session("acme", "sam", "sales").unwrap();
        let id = lead(&mut ws, &s, "ada@initech.com");
        assert_eq!(id, "lead-1");
        text(run(&mut ws, &s, "crm.lead.qualify", &[("id", &id)]).unwrap());
        let t = text(
            run(
                &mut ws,
                &s,
                "crm.lead.convert",
                &[("id", &id), ("deal", "Rollout"), ("amount", "1250.5")],
            )
            .unwrap(),
        );
        assert!(t.contains("Converted"), "{t}");
        let list = text(run(&mut ws, &s, "crm.opp.list", &[]).unwrap());
        assert!(
            list.contains("Rollout") && list.contains("1250.50 EUR"),
            "{list}"
        );
        assert_eq!(
            crm.borrow()
                .opportunities_of(&Ctx::new("acme", "x", &[], 0))
                .len(),
            1
        );
        assert!(ws.audit().verify().is_ok());
    }

    #[test]
    fn a_duplicate_lead_is_refused_with_the_existing_id() {
        let (mut ws, _) = setup();
        let s = ws.open_session("acme", "sam", "sales").unwrap();
        lead(&mut ws, &s, "ada@initech.com");
        let e = run(
            &mut ws,
            &s,
            "crm.lead.capture",
            &[
                ("name", "Ada"),
                ("email", "ADA@initech.com"),
                ("source", "web"),
            ],
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("duplicate of lead-1"), "{e}");
    }

    #[test]
    fn a_viewer_can_read_but_not_write() {
        let (mut ws, _) = setup();
        let v = ws.open_session("acme", "vic", "viewer").unwrap();
        assert!(text(run(&mut ws, &v, "crm.lead.list", &[]).unwrap()).contains("No leads"));
        let r = run(
            &mut ws,
            &v,
            "crm.lead.capture",
            &[("name", "A"), ("email", "a@b.co"), ("source", "web")],
        );
        assert!(!matches!(r, Ok(Reply::Done(_))), "{r:?}");
    }

    #[test]
    fn a_large_deal_is_held_and_needs_a_second_person_in_the_workspace() {
        let (mut ws, _) = setup();
        let s = ws.open_session("acme", "sam", "sales").unwrap();
        let id = lead(&mut ws, &s, "ada@initech.com");
        run(&mut ws, &s, "crm.lead.qualify", &[("id", &id)]).unwrap();
        // 60,000.00 is over the default 50,000.00 threshold.
        run(
            &mut ws,
            &s,
            "crm.lead.convert",
            &[("id", &id), ("deal", "Big"), ("amount", "60000")],
        )
        .unwrap();
        for to in ["qualified", "proposal", "negotiation"] {
            run(
                &mut ws,
                &s,
                "crm.opp.advance",
                &[("id", "opp-4"), ("to", to)],
            )
            .map_or_else(|e| panic!("{to}: {e}"), |_| ());
        }
        let t = text(
            run(
                &mut ws,
                &s,
                "crm.opp.advance",
                &[("id", "opp-4"), ("to", "won")],
            )
            .unwrap(),
        );
        assert!(t.contains("appr-5"), "{t}");
        // Deciding is consequential: the workspace holds it for another person.
        let held = run(
            &mut ws,
            &s,
            "crm.approval.decide",
            &[("approval", "appr-5"), ("decision", "approve")],
        )
        .unwrap();
        let Reply::AwaitingApproval { id: wid } = held else {
            panic!("{held:?}")
        };
        let boss = ws.open_session("acme", "olga", "owner").unwrap();
        let _ = boss;
        let done = ws.decide("acme", &wid, "olga", "owner", true).unwrap();
        let Reply::Done(o) = done else {
            panic!("{done:?}")
        };
        assert!(o.text.contains("ClosedWon"), "{}", o.text);
        let list = text(run(&mut ws, &s, "crm.opp.list", &[]).unwrap());
        assert!(list.contains("ClosedWon"), "{list}");
    }

    #[test]
    fn bad_input_is_refused_before_anything_changes() {
        for bad in [
            "",
            "abc",
            "1.234",
            "-5",
            "1,5",
            ".5",
            "99999999999999999999",
        ] {
            assert!(minor_units(bad).is_err(), "{bad}");
        }
        assert_eq!(minor_units("10").unwrap(), 1000);
        assert_eq!(minor_units("10.5").unwrap(), 1050);
        assert_eq!(minor_units("0.07").unwrap(), 7);
        assert!(stage_of("sideways").is_err());
        let (mut ws, _) = setup();
        let s = ws.open_session("acme", "sam", "sales").unwrap();
        assert!(run(
            &mut ws,
            &s,
            "crm.lead.capture",
            &[("name", "A"), ("email", "nope"), ("source", "web")]
        )
        .is_err());
        assert!(run(
            &mut ws,
            &s,
            "crm.lead.convert",
            &[("id", "lead-1"), ("deal", "x"), ("amount", "1.2.3")]
        )
        .is_err());
        assert!(text(run(&mut ws, &s, "crm.lead.list", &[]).unwrap()).contains("No leads"));
    }

    #[test]
    fn the_forecast_counts_open_and_won_money_exactly() {
        let (mut ws, _) = setup();
        let s = ws.open_session("acme", "sam", "sales").unwrap();
        assert!(text(run(&mut ws, &s, "crm.forecast", &[]).unwrap()).contains("No deals"));
        let id = lead(&mut ws, &s, "ada@initech.com");
        run(&mut ws, &s, "crm.lead.qualify", &[("id", &id)]).unwrap();
        run(
            &mut ws,
            &s,
            "crm.lead.convert",
            &[("id", &id), ("deal", "D"), ("amount", "100.10")],
        )
        .unwrap();
        let f = text(run(&mut ws, &s, "crm.forecast", &[]).unwrap());
        assert!(f.contains("open 100.10 EUR"), "{f}");
    }
}
