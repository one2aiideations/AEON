//! Signing in with a passphrase, and managing accounts.
//!
//! While accounts exist the device starts locked: only the conversation takes keys and only
//! `/signin <name>` does anything. Sign-in asks for the passphrase in hidden entry (it is never
//! shown, kept in the conversation, written to the audit log or saved as a draft); the account's
//! stored role is used, never one the person types. A wrong passphrase is a refusal in the audit
//! log, so five of them lock the name out until someone who can approve types `/unlock`.
//!
//! Managing accounts needs the `users.admin` permission (the owner has it): `/passwd`, `/users`,
//! `/adduser`, `/setrole`, `/deluser`, `/resetpass`.
//!
//! What this does not show: it protects the device from someone at the keyboard who does not know
//! a passphrase. It does not hide the other windows' contents on a desktop that was already open,
//! and a person who can read the data disk can try passphrases offline against the stored
//! stretches. If every owner forgets their passphrase, the accounts file on the data disk has to
//! be removed by hand.

use super::accounts::{valid_name, Verdict};
use super::{Backend, Device, Outcome, OWNER};
use mouli_screen::screen::Action;

/// The name shown while nobody is signed in.
const LOCKED_USER: &str = "(locked)";

impl<B: Backend> Device<B> {
    /// True when an accounts file exists with accounts in it, or exists but cannot be read (the
    /// device then stays locked rather than open).
    #[must_use]
    pub fn has_accounts(&self) -> bool {
        self.accounts.as_ref().is_some_and(|a| !a.is_empty()) || self.accounts_error.is_some()
    }

    /// True while nobody is signed in.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Locks the device if there are accounts: the conversation is cleared and only `/signin` is
    /// accepted until someone signs in. Does nothing when there are no accounts.
    pub fn lock(&mut self) {
        if !self.has_accounts() {
            return;
        }
        self.locked = true;
        self.shell.active = mouli_screen::screen::Tab::Chat;
        self.user = LOCKED_USER.into();
        self.shell.chat.clear();
        let msg = match &self.accounts_error {
            Some(e) => format!("Locked: the accounts file cannot be read ({e}). Fix or remove it on the data disk."),
            None => "Locked. Type /signin <name> and then your passphrase.".into(),
        };
        self.notice(&msg);
        self.refresh();
    }

    fn can_admin(&self) -> bool {
        self.ws.role_can(&self.role, "users.admin")
    }

    /// Writes a refusal into the audit chain and tells the person.
    fn refuse_account(&mut self, action: &str, subject: &str, why: &str) {
        self.ws
            .record_event(&self.user, action, subject, &format!("denied: {why}"));
        self.notice(&format!("Not allowed: {why}."));
    }

    pub(super) fn perform_locked(&mut self, action: Action) -> Outcome {
        match action {
            Action::None | Action::Show(_) => Outcome::Handled,
            Action::Quit => Outcome::Quit,
            Action::Reboot => Outcome::Reboot,
            Action::Secret { token, text } => {
                self.secret_entered(&token, text.expose());
                Outcome::Handled
            }
            Action::Send(text) if text.trim_start().starts_with("/signin") => {
                self.send(&text);
                Outcome::Handled
            }
            _ => {
                self.notice("Locked. Type /signin <name> and then your passphrase.");
                Outcome::Handled
            }
        }
    }

    /// `/signin ...`. With accounts: `/signin <name>`, then the passphrase in hidden entry.
    /// Without: the open form `/signin <name> <role>`.
    pub(super) fn signin_command(&mut self, rest: &str) {
        let words: Vec<&str> = rest.split_whitespace().collect();
        if self.has_accounts() {
            let [name] = words[..] else {
                self.notice("Type /signin <name> and then your passphrase.");
                return;
            };
            if !valid_name(name) {
                self.notice("Type /signin <name> and then your passphrase.");
                return;
            }
            if self.ws.is_locked_out(name) {
                self.ws
                    .record_event(name, "signin", "-", "denied: locked out");
                self.notice(&format!(
                    "{name} is locked out after too many wrong passphrases. Someone who can approve must type /unlock {name}."
                ));
                return;
            }
            if self.accounts.is_none() {
                self.notice("The accounts file cannot be read, so nobody can sign in.");
                return;
            }
            self.shell
                .chat
                .begin_secret(&format!("signin:{name}"), &format!("Passphrase for {name}"));
            return;
        }
        match (words.first(), words.get(1), words.get(2)) {
            (Some(name), Some(role), None) => match self.switch_user(name, role) {
                Ok(()) => self.notice(&format!(
                    "Signed in as {name} ({role}). Nobody is asked for a passphrase yet: type /passwd to set yours and require sign-in."
                )),
                Err(e) => self.notice(&format!("Could not sign in: {e}")),
            },
            _ => self.notice("Usage: /signin <name> <role>"),
        }
    }

    /// Handles the account commands. Returns whether `text` was one.
    pub(super) fn account_command(&mut self, text: &str) -> bool {
        let mut w = text.split_whitespace();
        let Some(cmd) = w.next() else { return false };
        let args: Vec<&str> = w.collect();
        match cmd {
            "/passwd" => self.cmd_passwd(&args),
            "/users" => self.cmd_users(),
            "/adduser" => self.cmd_adduser(&args),
            "/setrole" => self.cmd_setrole(&args),
            "/deluser" => self.cmd_deluser(&args),
            "/resetpass" => self.cmd_resetpass(&args),
            _ => return false,
        }
        true
    }

    fn accounts_unavailable(&mut self) -> bool {
        if self.accounts.is_none() {
            self.notice("Accounts are kept on the data disk, and there is none.");
            return true;
        }
        false
    }

    fn cmd_passwd(&mut self, args: &[&str]) {
        if !args.is_empty() {
            self.notice("Type /passwd and then the new passphrase (it is not shown).");
            return;
        }
        if self.accounts_unavailable() {
            return;
        }
        let has_account = self
            .accounts
            .as_ref()
            .is_some_and(|a| a.role_of(&self.user).is_some());
        let empty = self.accounts.as_ref().is_some_and(|a| a.is_empty());
        if empty && !self.can_admin() {
            let user = self.user.clone();
            self.refuse_account(
                "accounts.admin",
                &user,
                "only someone with the users.admin permission can set up the first account",
            );
            return;
        }
        if !empty && !has_account {
            self.notice("You have no account. Someone with the users.admin permission can add one with /adduser.");
            return;
        }
        self.shell.chat.begin_secret(
            "passwd",
            &format!(
                "New passphrase for {} (at least {} characters)",
                self.user,
                super::accounts::MIN_PASSPHRASE
            ),
        );
    }

    fn cmd_users(&mut self) {
        if !self.can_admin() {
            self.refuse_account(
                "accounts.admin",
                "list",
                "listing accounts needs the users.admin permission",
            );
            return;
        }
        let Some(a) = &self.accounts else {
            self.accounts_unavailable();
            return;
        };
        let list = a.list();
        let text = if list.is_empty() {
            "No accounts yet. Type /passwd to set up yours.".to_string()
        } else {
            let mut t = String::from("Accounts:");
            for (name, role) in list {
                let lock = if self.ws.is_locked_out(&name) {
                    "  (locked out)"
                } else {
                    ""
                };
                t.push_str(&format!("\n  {name}  {role}{lock}"));
            }
            t
        };
        self.notice(&text);
    }

    /// Checks that `role` is a role this workspace defines.
    fn known_role(&self, role: &str) -> bool {
        self.ws.role_names().contains(&role)
    }

    fn cmd_adduser(&mut self, args: &[&str]) {
        let [name, role] = args[..] else {
            self.notice("Usage: /adduser <name> <role>   (then the passphrase is asked for)");
            return;
        };
        if !self.can_admin() {
            self.refuse_account(
                "accounts.admin",
                name,
                "adding an account needs the users.admin permission",
            );
            return;
        }
        if self.accounts_unavailable() {
            return;
        }
        if !valid_name(name) {
            self.notice("Names use 1 to 32 letters, digits, . - and _.");
            return;
        }
        if !self.known_role(role) {
            let roles = self.ws.role_names().join(", ");
            self.notice(&format!("There is no role {role}. Roles: {roles}."));
            return;
        }
        if self
            .accounts
            .as_ref()
            .is_some_and(|a| a.role_of(name).is_some())
        {
            self.notice(&format!(
                "{name} already has an account. Use /resetpass {name} or /setrole {name} <role>."
            ));
            return;
        }
        self.shell.chat.begin_secret(
            &format!("adduser:{name}:{role}"),
            &format!("Passphrase for the new account {name}"),
        );
    }

    fn cmd_setrole(&mut self, args: &[&str]) {
        let [name, role] = args[..] else {
            self.notice("Usage: /setrole <name> <role>");
            return;
        };
        if !self.can_admin() {
            self.refuse_account(
                "accounts.admin",
                name,
                "changing a role needs the users.admin permission",
            );
            return;
        }
        if self.accounts_unavailable() {
            return;
        }
        if !self.known_role(role) {
            let roles = self.ws.role_names().join(", ");
            self.notice(&format!("There is no role {role}. Roles: {roles}."));
            return;
        }
        let result = self
            .accounts
            .as_mut()
            .map(|a| a.set_role(name, role, OWNER));
        match result {
            Some(Ok(())) => {
                self.audit_event("account.role", &format!("{name}|{role}"), "ok");
                self.notice(&format!(
                    "{name} now has the {role} role (from their next sign-in)."
                ));
            }
            Some(Err(e)) => self.notice(&format!("Not changed: {e}.")),
            None => {}
        }
    }

    fn cmd_deluser(&mut self, args: &[&str]) {
        let [name] = args[..] else {
            self.notice("Usage: /deluser <name>");
            return;
        };
        if !self.can_admin() {
            self.refuse_account(
                "accounts.admin",
                name,
                "removing an account needs the users.admin permission",
            );
            return;
        }
        if self.accounts_unavailable() {
            return;
        }
        let result = self.accounts.as_mut().map(|a| a.remove(name, OWNER));
        match result {
            Some(Ok(())) => {
                self.audit_event("account.remove", name, "ok");
                self.notice(&format!("{name} can no longer sign in."));
            }
            Some(Err(e)) => self.notice(&format!("Not removed: {e}.")),
            None => {}
        }
    }

    fn cmd_resetpass(&mut self, args: &[&str]) {
        let [name] = args[..] else {
            self.notice("Usage: /resetpass <name>   (then the new passphrase is asked for)");
            return;
        };
        if !self.can_admin() {
            self.refuse_account(
                "accounts.admin",
                name,
                "resetting a passphrase needs the users.admin permission",
            );
            return;
        }
        if self.accounts_unavailable() {
            return;
        }
        if !self
            .accounts
            .as_ref()
            .is_some_and(|a| a.role_of(name).is_some())
        {
            self.notice(&format!("There is no account called {name}."));
            return;
        }
        self.shell.chat.begin_secret(
            &format!("resetpass:{name}"),
            &format!("New passphrase for {name}"),
        );
    }

    /// A hidden line came back. `token` says what it was asked for; `text` is never logged.
    pub(super) fn secret_entered(&mut self, token: &str, text: &str) {
        let mut parts = token.splitn(3, ':');
        let kind = parts.next().unwrap_or("");
        let first = parts.next().unwrap_or("");
        let second = parts.next().unwrap_or("");
        match kind {
            "signin" => self.finish_signin(first, text),
            "passwd" => {
                let user = self.user.clone();
                let role = self
                    .accounts
                    .as_ref()
                    .and_then(|a| a.role_of(&user).map(str::to_string))
                    .unwrap_or_else(|| self.role.clone());
                self.store_passphrase(&user, &role, text, "passphrase.set");
            }
            "adduser" => {
                // Asked for by an administrator: check again, the permission may have changed.
                if !self.can_admin() {
                    self.refuse_account(
                        "accounts.admin",
                        first,
                        "adding an account needs the users.admin permission",
                    );
                    return;
                }
                self.store_passphrase(first, second, text, "account.add");
            }
            "resetpass" => {
                if !self.can_admin() {
                    self.refuse_account(
                        "accounts.admin",
                        first,
                        "resetting a passphrase needs the users.admin permission",
                    );
                    return;
                }
                let role = self
                    .accounts
                    .as_ref()
                    .and_then(|a| a.role_of(first).map(str::to_string));
                match role {
                    Some(role) => self.store_passphrase(first, &role, text, "passphrase.reset"),
                    None => self.notice(&format!("There is no account called {first}.")),
                }
            }
            _ => self.notice("That entry was not asked for."),
        }
    }

    fn store_passphrase(&mut self, name: &str, role: &str, passphrase: &str, audit: &str) {
        let Some(a) = self.accounts.as_mut() else {
            self.notice("Accounts are kept on the data disk, and there is none.");
            return;
        };
        match a.set(name, role, passphrase, OWNER) {
            Ok(()) => {
                let subject = if audit == "account.add" {
                    format!("{name}|{role}")
                } else {
                    name.to_string()
                };
                self.audit_event(audit, &subject, "ok");
                self.notice(&format!(
                    "Saved. {name} signs in with /signin {name} and the new passphrase."
                ));
            }
            Err(e) => self.notice(&format!("Not saved: {e}.")),
        }
    }

    fn finish_signin(&mut self, name: &str, passphrase: &str) {
        if self.ws.is_locked_out(name) {
            self.ws
                .record_event(name, "signin", "-", "denied: locked out");
            self.notice(&format!(
                "{name} is locked out after too many wrong passphrases. Someone who can approve must type /unlock {name}."
            ));
            return;
        }
        let verdict = match &self.accounts {
            Some(a) => a.verify(name, passphrase),
            None => Verdict::Wrong,
        };
        match verdict {
            Verdict::Ok(role) => match self.switch_user(name, &role) {
                Ok(()) => {
                    self.locked = false;
                    self.shell.active = mouli_screen::screen::Tab::Chat;
                    self.notice(&format!("Signed in as {name} ({role})."));
                }
                Err(e) => self.notice(&format!("Could not sign in: {e}")),
            },
            Verdict::Wrong => {
                self.ws
                    .record_event(name, "signin", "-", "denied: wrong passphrase");
                self.notice("Wrong passphrase, or there is no such account.");
            }
        }
    }
}
