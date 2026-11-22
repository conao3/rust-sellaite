use std::path::PathBuf;

use async_graphql::{
    ComplexObject, Context, EmptySubscription, Enum, Error, InputObject, Object, Result, Schema,
    SimpleObject,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{dashboard::SharedControl, runtime};

pub(crate) struct Connection(pub String);

pub type DashboardSchema = Schema<Query, Mutation, EmptySubscription>;

pub fn schema() -> DashboardSchema {
    schema_with_control(SharedControl::default())
}

pub(crate) fn schema_with_control(control: SharedControl) -> DashboardSchema {
    Schema::build(Query, Mutation, EmptySubscription)
        .data(control)
        .limit_depth(12)
        .limit_complexity(200)
        .finish()
}

#[derive(Serialize, SimpleObject)]
#[serde(rename_all = "PascalCase")]
#[graphql(complex)]
pub struct Desktop {
    id: String,
    width: Option<u32>,
    height: Option<u32>,
    pid: Option<u32>,
    paused: bool,
    taking_over: bool,
    paused_reason: Option<String>,
    user_seat_idle: bool,
    pending_approval: Option<Approval>,
    pending_notes: usize,
}

#[derive(Serialize, Deserialize, SimpleObject)]
#[serde(rename_all = "PascalCase")]
struct Approval {
    id: String,
    tool: String,
    reason: String,
    domain: Option<String>,
    target: Target,
    bounds: Bounds,
    x: Option<u32>,
    y: Option<u32>,
}

#[derive(Serialize, Deserialize, SimpleObject)]
#[serde(rename_all = "PascalCase")]
struct Target {
    role: String,
    name: String,
}

#[derive(Serialize, Deserialize, SimpleObject)]
#[serde(rename_all = "PascalCase")]
struct Bounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Desktop {
    pub fn read(id: &str, dir: &std::path::Path) -> Self {
        let state = runtime::read_state(dir);
        let paused_reason = runtime::paused(dir);
        Self {
            id: id.to_owned(),
            width: state.as_ref().map(|s| s.width),
            height: state.as_ref().map(|s| s.height),
            pid: state.as_ref().map(|s| s.pid),
            paused: paused_reason.is_some(),
            taking_over: runtime::taking_over(dir),
            paused_reason,
            user_seat_idle: runtime::user_idle(dir),
            pending_approval: crate::approval::pending(dir)
                .and_then(|value| serde_json::from_value(value).ok()),
            pending_notes: runtime::pending_notes(dir),
        }
    }

    pub fn list() -> Vec<Self> {
        runtime::live_desktops()
            .iter()
            .map(|(id, dir, _)| Self::read(id, dir))
            .collect()
    }
}

#[ComplexObject]
impl Desktop {
    async fn owns_takeover(&self, ctx: &Context<'_>) -> bool {
        let Some(connection) = ctx.data_opt::<Connection>() else {
            return false;
        };
        ctx.data_unchecked::<SharedControl>()
            .lock()
            .await
            .owns(&self.id, &connection.0)
    }
}

pub struct Query;

#[Object]
impl Query {
    async fn desktops(&self) -> Vec<Desktop> {
        Desktop::list()
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Enum)]
enum ApprovalAction {
    Approve,
    Reject,
    Edit,
}

#[derive(InputObject)]
struct ApprovalDecision {
    action: ApprovalAction,
    x: Option<u32>,
    y: Option<u32>,
    text: Option<String>,
}

pub struct Mutation;

fn desktop_dir(id: &str) -> Result<PathBuf> {
    if !runtime::is_id(id) {
        return Err(Error::new("invalid desktop Id"));
    }
    runtime::live_dir(id).ok_or_else(|| Error::new("no such desktop"))
}

#[Object]
impl Mutation {
    async fn pause_desktop(&self, id: String) -> Result<Desktop> {
        let dir = desktop_dir(&id)?;
        runtime::pause(&dir, "the user pressed Stop in the dashboard")?;
        Ok(Desktop::read(&id, &dir))
    }

    async fn resume_desktop(&self, ctx: &Context<'_>, id: String) -> Result<Desktop> {
        let dir = desktop_dir(&id)?;
        let _control = ctx.data_unchecked::<SharedControl>().lock().await;
        if runtime::taking_over(&dir) {
            return Err(Error::new("desktop takeover state changed"));
        }
        runtime::resume(&dir)?;
        Ok(Desktop::read(&id, &dir))
    }

    async fn take_over_desktop(&self, ctx: &Context<'_>, id: String) -> Result<Desktop> {
        let dir = desktop_dir(&id)?;
        let connection = ctx.data::<Connection>()?;
        ctx.data_unchecked::<SharedControl>()
            .lock()
            .await
            .take(&id, &dir, &connection.0)?;
        Ok(Desktop::read(&id, &dir))
    }

    async fn give_back_desktop(&self, ctx: &Context<'_>, id: String) -> Result<Desktop> {
        let dir = desktop_dir(&id)?;
        let connection = ctx.data::<Connection>()?;
        ctx.data_unchecked::<SharedControl>()
            .lock()
            .await
            .release(&id, &dir, &connection.0)?;
        Ok(Desktop::read(&id, &dir))
    }

    async fn create_note(&self, id: String, text: String) -> Result<Desktop> {
        let dir = desktop_dir(&id)?;
        runtime::check_note(&text)?;
        runtime::add_note(&dir, &text)?;
        Ok(Desktop::read(&id, &dir))
    }

    async fn decide_approval(
        &self,
        id: String,
        request_id: String,
        decision: ApprovalDecision,
    ) -> Result<bool> {
        let dir = desktop_dir(&id)?;
        if request_id.is_empty() || !request_id.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::new("invalid approval Id"));
        }
        let action = match decision.action {
            ApprovalAction::Approve => "approve",
            ApprovalAction::Reject => "reject",
            ApprovalAction::Edit => "edit",
        };
        crate::approval::decide(
            &dir,
            &request_id,
            &json!({"Action": action, "X": decision.x, "Y": decision.y, "Text": decision.text}),
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_status_reads_pause_idle_and_notes_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        runtime::pause(dir.path(), "paused for review").unwrap();
        runtime::set_user_idle(dir.path(), true);
        runtime::add_note(dir.path(), "review this").unwrap();

        let desktop = Desktop::read("Ab1234", dir.path());
        assert_eq!(desktop.id, "Ab1234");
        assert!(desktop.paused);
        assert_eq!(desktop.paused_reason.as_deref(), Some("paused for review"));
        assert!(desktop.user_seat_idle);
        assert_eq!(desktop.pending_notes, 1);
    }
}
