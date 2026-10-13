use crate::types::*;
use anyhow::{Result, bail};
use fs2::FileExt;
use llm_contract::{
    ContentPart, GenerateResponse, Message, Role, ToolCall, ToolOutcome, ToolResult,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{fs::File, path::Path, sync::Mutex};

pub struct Store {
    connection: Mutex<Connection>,
    _lock: File,
}
impl Store {
    pub fn claim_work(
        &self,
        root: &str,
        owner: &str,
        resource: &str,
        seconds: i64,
    ) -> Result<Value> {
        if resource.is_empty() || resource.len() > 500 || !(1..=3600).contains(&seconds) {
            bail!("invalid work claim")
        }
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS work_claims(root TEXT NOT NULL,resource TEXT NOT NULL,owner TEXT NOT NULL,until INTEGER NOT NULL,PRIMARY KEY(root,resource));")?;
        tx.execute("INSERT INTO work_claims(root,resource,owner,until) VALUES(?1,?2,?3,?4) ON CONFLICT(root,resource) DO UPDATE SET owner=excluded.owner,until=excluded.until WHERE work_claims.until<=?5 OR work_claims.owner=excluded.owner",params![root,resource,owner,now()+seconds,now()])?;
        let (holder, until): (String, i64) = tx.query_row(
            "SELECT owner,until FROM work_claims WHERE root=?1 AND resource=?2",
            params![root, resource],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.commit()?;
        Ok(json!({"acquired":holder==owner,"owner":holder,"until":until,"resource":resource}))
    }
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("another daemon owns this database"))?;
        let c = Connection::open(path)?;
        c.pragma_update(None, "journal_mode", "WAL")?;
        c.pragma_update(None, "synchronous", "FULL")?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS agents(id TEXT PRIMARY KEY, root TEXT NOT NULL, parent TEXT, status TEXT NOT NULL, due INTEGER NOT NULL, lease INTEGER NOT NULL DEFAULT 0, data TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT, agent TEXT NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL, time INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS actions(agent TEXT NOT NULL, call TEXT NOT NULL, state TEXT NOT NULL, data TEXT NOT NULL, outcome TEXT, PRIMARY KEY(agent,call));
          CREATE TABLE IF NOT EXISTS envelopes(id TEXT PRIMARY KEY, kind TEXT NOT NULL, recipient TEXT, topic TEXT, data TEXT NOT NULL, is_read INTEGER NOT NULL DEFAULT 0, time INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS outbox(id TEXT PRIMARY KEY, peer TEXT NOT NULL, data TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', due INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS routes(id TEXT PRIMARY KEY, data TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS archives(id INTEGER PRIMARY KEY AUTOINCREMENT, agent TEXT NOT NULL, data TEXT NOT NULL, time INTEGER NOT NULL);")?;
        // Exclusive process ownership makes abandoned leases and in-flight side effects identifiable.
        let uncertain = serde_json::to_string(&ToolOutcome::Failure { code:"interrupted_uncertain".into(), message:"Runtime restarted while the action was in progress. Inspect its artifacts before retrying; it was not automatically re-executed.".into() })?;
        c.execute(
            "UPDATE actions SET state='done',outcome=?1 WHERE state='running'",
            [&uncertain],
        )?;
        c.execute(
            "UPDATE agents SET lease=0 WHERE status NOT IN ('completed','stopped')",
            [],
        )?;
        Ok(Self {
            connection: Mutex::new(c),
            _lock: lock,
        })
    }
    pub fn create(
        &self,
        create: CreateGoal,
        parent: Option<&Goal>,
        default_route: &str,
    ) -> Result<Goal> {
        if create.goal.trim().is_empty()
            || create.name.trim().is_empty()
            || create.goal.len() > 32_000
            || create.name.len() > 200
        {
            bail!("invalid goal");
        }
        let ident = id();
        let goal = Goal {
            id: ident.clone(),
            root_id: parent.map(|p| p.root_id.clone()).unwrap_or(ident),
            parent_id: parent.map(|p| p.id.clone()),
            name: create.name,
            continuous: create.continuous || parent.is_some_and(|p| p.continuous),
            global_goal: parent
                .map(|p| p.global_goal.clone())
                .unwrap_or_else(|| create.goal.clone()),
            goal: create.goal,
            route_id: create.route_id.unwrap_or_else(|| {
                parent
                    .map(|p| p.route_id.clone())
                    .unwrap_or(default_route.into())
            }),
            status: "queued".into(),
            due: now(),
            steps: 0,
            errors: 0,
            verifier: create.verifier,
            snapshot: Snapshot::default(),
            created: now(),
            updated: now(),
        };
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let active: i64 = tx.query_row(
            "SELECT count(*) FROM agents WHERE status NOT IN ('completed','stopped')",
            [],
            |r| r.get(0),
        )?;
        if active >= 8 {
            bail!(
                "node active-agent capacity is 8; finish or stop redundant agents before expanding"
            );
        }
        tx.execute(
            "INSERT INTO agents(id,root,parent,status,due,data) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                goal.id,
                goal.root_id,
                goal.parent_id,
                goal.status,
                goal.due,
                serde_json::to_string(&goal)?
            ],
        )?;
        tx.execute(
            "INSERT INTO events(agent,kind,data,time) VALUES(?1,'created',?2,?3)",
            params![
                goal.id,
                json!({"name":goal.name,"parent":goal.parent_id}).to_string(),
                now()
            ],
        )?;
        tx.commit()?;
        Ok(goal)
    }
    pub fn get(&self, ident: &str) -> Result<Goal> {
        let c = self.connection.lock().unwrap();
        let (data, status, due): (String, String, i64) = c.query_row(
            "SELECT data,status,due FROM agents WHERE id=?1",
            [ident],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let mut g: Goal = serde_json::from_str(&data)?;
        g.status = status;
        g.due = due;
        Ok(g)
    }
    pub fn list(&self) -> Result<Vec<Goal>> {
        let c = self.connection.lock().unwrap();
        let mut stmt = c.prepare("SELECT data,status,due FROM agents ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (d, s, due) = r?;
            let mut g: Goal = serde_json::from_str(&d)?;
            g.status = s;
            g.due = due;
            Ok(g)
        })
        .collect()
    }
    pub fn claim(&self) -> Result<Option<Goal>> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let data:Option<String>=tx.query_row("SELECT data FROM agents WHERE status IN ('queued','running','waiting','retrying') AND due<=?1 AND lease<=?1 ORDER BY due,rowid LIMIT 1",[now()],|r|r.get(0)).optional()?;
        let Some(data) = data else { return Ok(None) };
        let mut g: Goal = serde_json::from_str(&data)?;
        if g.root_id != g.id {
            let root_status: String =
                tx.query_row("SELECT status FROM agents WHERE id=?1", [&g.root_id], |r| {
                    r.get(0)
                })?;
            if root_status == "stopped" || root_status == "completed" {
                tx.execute(
                    "UPDATE agents SET status='stopped',lease=0 WHERE id=?1",
                    [&g.id],
                )?;
                tx.commit()?;
                return Ok(None);
            }
        }
        g.status = "running".into();
        g.updated = now();
        tx.execute(
            "UPDATE agents SET status='running',lease=?2,data=?3 WHERE id=?1",
            params![g.id, now() + 3600, serde_json::to_string(&g)?],
        )?;
        tx.commit()?;
        Ok(Some(g))
    }
    pub fn save(&self, goal: &Goal, status: &str, due: i64) -> Result<()> {
        let mut value = goal.clone();
        value.status = status.into();
        value.due = due;
        value.updated = now();
        let c = self.connection.lock().unwrap();
        if status == "waiting" {
            let mut statement =
                c.prepare("SELECT id FROM envelopes WHERE recipient=?1 AND is_read=0")?;
            let rows = statement.query_map([&goal.id], |row| row.get::<_, String>(0))?;
            for row in rows {
                if !goal.snapshot.inbox_seen.contains(&row?) {
                    value.due = now();
                    break;
                }
            }
        }
        c.execute("UPDATE agents SET data=?2,status=?3,due=?4,lease=0 WHERE id=?1 AND status NOT IN ('stopped','completed')",params![goal.id,serde_json::to_string(&value)?,status,value.due])?;
        Ok(())
    }
    pub fn checkpoint(&self, agent: &str, state: Value) -> Result<()> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let data: String =
            tx.query_row("SELECT data FROM agents WHERE id=?1", [agent], |r| r.get(0))?;
        let mut goal: Goal = serde_json::from_str(&data)?;
        goal.snapshot.work_state = state;
        tx.execute(
            "UPDATE agents SET data=?2 WHERE id=?1",
            params![agent, serde_json::to_string(&goal)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn stop(&self, ident: &str) -> Result<()> {
        self.connection.lock().unwrap().execute(
            "WITH RECURSIVE descendants(id) AS (SELECT id FROM agents WHERE id=?1 UNION ALL SELECT agents.id FROM agents JOIN descendants ON agents.parent=descendants.id) UPDATE agents SET status='stopped',lease=0 WHERE id IN (SELECT id FROM descendants)",
            [ident],
        )?;
        self.event(ident, "stopped", json!({}))
    }
    pub fn resume(&self, ident: &str) -> Result<()> {
        self.connection.lock().unwrap().execute(
            "UPDATE agents SET status='queued',due=?2,lease=0 WHERE id=?1 AND status='stopped'",
            params![ident, now()],
        )?;
        self.event(ident, "resumed", json!({}))
    }
    pub fn event(&self, agent: &str, kind: &str, data: Value) -> Result<()> {
        self.connection.lock().unwrap().execute(
            "INSERT INTO events(agent,kind,data,time) VALUES(?1,?2,?3,?4)",
            params![agent, kind, data.to_string(), now()],
        )?;
        Ok(())
    }
    pub fn events(&self, agent: &str, after: i64) -> Result<Vec<Value>> {
        let c = self.connection.lock().unwrap();
        let mut stmt=c.prepare("SELECT seq,kind,data,time FROM events WHERE agent=?1 AND seq>?2 ORDER BY seq LIMIT 200")?;
        let rows = stmt.query_map(params![agent, after], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        rows.map(|r|{let(seq,kind,data,time)=r?;Ok(json!({"seq":seq,"kind":kind,"data":serde_json::from_str::<Value>(&data)?,"time":time}))}).collect()
    }
    pub fn model_response(&self, goal: &mut Goal, response: GenerateResponse) -> Result<()> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        for part in &response.message.content {
            if let ContentPart::ToolCall(call) = part {
                let previous: Option<String> = tx
                    .query_row(
                        "SELECT data FROM actions WHERE agent=?1 AND call=?2",
                        params![goal.id, call.id],
                        |r| r.get(0),
                    )
                    .optional()?;
                let encoded = serde_json::to_string(call)?;
                if let Some(previous) = previous {
                    if previous != encoded {
                        bail!("tool call ID reused with changed arguments");
                    }
                } else {
                    tx.execute(
                        "INSERT INTO actions(agent,call,state,data) VALUES(?1,?2,'queued',?3)",
                        params![goal.id, call.id, encoded],
                    )?;
                }
            }
        }
        tx.execute(
            "INSERT INTO events(agent,kind,data,time) VALUES(?1,'model_response',?2,?3)",
            params![goal.id, serde_json::to_string(&response)?, now()],
        )?;
        goal.snapshot.messages.push(response.message);
        goal.steps += 1;
        goal.updated = now();
        tx.execute(
            "UPDATE agents SET data=?2 WHERE id=?1",
            params![goal.id, serde_json::to_string(goal)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn pending(&self, agent: &str) -> Result<Vec<(ToolCall, String, Option<ToolOutcome>)>> {
        let c = self.connection.lock().unwrap();
        let mut stmt=c.prepare("SELECT data,state,outcome FROM actions WHERE agent=?1 AND state!='delivered' ORDER BY rowid")?;
        let rows = stmt.query_map([agent], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (d, s, o) = r?;
            Ok((
                serde_json::from_str(&d)?,
                s,
                o.map(|v| serde_json::from_str(&v)).transpose()?,
            ))
        })
        .collect()
    }
    pub fn action_running(&self, agent: &str, call: &str) -> Result<()> {
        self.connection.lock().unwrap().execute(
            "UPDATE actions SET state='running' WHERE agent=?1 AND call=?2 AND state='queued'",
            params![agent, call],
        )?;
        Ok(())
    }
    pub fn action_done(&self, agent: &str, call: &str, outcome: &ToolOutcome) -> Result<()> {
        self.connection.lock().unwrap().execute(
            "UPDATE actions SET state='done',outcome=?3 WHERE agent=?1 AND call=?2",
            params![agent, call, serde_json::to_string(outcome)?],
        )?;
        self.event(
            agent,
            "tool_done",
            json!({"call_id":call,"outcome":outcome}),
        )
    }
    pub fn deliver(&self, goal: &mut Goal) -> Result<()> {
        let pending = self.pending(&goal.id)?;
        if pending.is_empty() {
            return Ok(());
        }
        if pending.iter().any(|(_, s, _)| s != "done") {
            bail!("incomplete tool batch");
        }
        let content = pending
            .iter()
            .map(|(call, _, outcome)| {
                ContentPart::ToolResult(ToolResult {
                    call_id: call.id.clone(),
                    outcome: outcome.clone().expect("done outcome"),
                })
            })
            .collect();
        goal.snapshot.messages.push(Message {
            role: Role::Tool,
            content,
            provider_state: None,
        });
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "UPDATE actions SET state='delivered' WHERE agent=?1 AND state='done'",
            [&goal.id],
        )?;
        tx.execute(
            "UPDATE agents SET data=?2 WHERE id=?1",
            params![goal.id, serde_json::to_string(goal)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn archive(&self, goal: &mut Goal, summary: Value) -> Result<()> {
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO archives(agent,data,time) VALUES(?1,?2,?3)",
            params![
                goal.id,
                serde_json::to_string(&goal.snapshot.messages)?,
                now()
            ],
        )?;
        goal.snapshot.archive_seq = tx.last_insert_rowid() as u64;
        goal.snapshot.summary = summary;
        goal.snapshot.messages.clear();
        tx.execute(
            "UPDATE agents SET data=?2 WHERE id=?1",
            params![goal.id, serde_json::to_string(goal)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn recall(&self, agent: &str, archive: i64, offset: usize, limit: usize) -> Result<Value> {
        let c = self.connection.lock().unwrap();
        let data: String = c.query_row(
            "SELECT data FROM archives WHERE id=?1 AND agent=?2",
            params![archive, agent],
            |r| r.get(0),
        )?;
        let text = data
            .chars()
            .skip(offset)
            .take(limit.min(64000))
            .collect::<String>();
        let next = offset + text.chars().count();
        Ok(
            json!({"archive_id":archive,"text":text,"offset":offset,"next_offset":next,"total_chars":data.chars().count()}),
        )
    }
    pub fn receive(&self, envelope: &Envelope) -> Result<bool> {
        if envelope.version != 1
            || !["dm", "board"].contains(&envelope.kind.as_str())
            || envelope.title.len() > 200
            || serde_json::to_vec(envelope)?.len() > 128_000
        {
            bail!("invalid envelope");
        }
        if envelope.kind == "dm" {
            self.get(
                envelope
                    .to_agent
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("missing recipient"))?,
            )?;
        }
        let mut c = self.connection.lock().unwrap();
        let tx = c.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT data FROM envelopes WHERE id=?1",
                [&envelope.id],
                |r| r.get(0),
            )
            .optional()?;
        let data = serde_json::to_string(envelope)?;
        if let Some(existing) = existing {
            if existing != data {
                bail!("envelope ID conflict");
            }
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO envelopes(id,kind,recipient,topic,data,time) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                envelope.id,
                envelope.kind,
                envelope.to_agent,
                envelope.topic,
                data,
                now()
            ],
        )?;
        if let Some(agent) = &envelope.to_agent {
            tx.execute(
                "UPDATE agents SET due=MIN(due,?2) WHERE id=?1 AND status IN ('queued','waiting')",
                params![agent, now()],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }
    pub fn heads(&self, agent: &str) -> Result<Vec<Value>> {
        self.envelopes(Some(agent), None, true)
            .map(|v| v.iter().map(Envelope::head).collect())
    }
    pub fn board_heads(&self) -> Result<Vec<Value>> {
        let c = self.connection.lock().unwrap();
        let mut stmt = c.prepare(
            "SELECT data FROM envelopes WHERE kind='board' ORDER BY time DESC,rowid DESC LIMIT 20",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str::<Envelope>(&r?)?.head()))
            .collect()
    }
    pub fn envelopes(
        &self,
        agent: Option<&str>,
        topic: Option<&str>,
        unread: bool,
    ) -> Result<Vec<Envelope>> {
        let c = self.connection.lock().unwrap();
        let mut stmt=c.prepare("SELECT data FROM envelopes WHERE (?1 IS NULL OR recipient=?1) AND (?2 IS NULL OR topic=?2) AND (?3=0 OR is_read=0) ORDER BY time DESC,rowid DESC LIMIT 50")?;
        let rows = stmt.query_map(params![agent, topic, unread as i64], |r| {
            r.get::<_, String>(0)
        })?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }
    pub fn read_message(&self, agent: &str, ident: &str) -> Result<Envelope> {
        let c = self.connection.lock().unwrap();
        let data: String = c.query_row(
            "SELECT data FROM envelopes WHERE id=?1 AND (recipient=?2 OR kind='board')",
            params![ident, agent],
            |r| r.get(0),
        )?;
        c.execute(
            "UPDATE envelopes SET is_read=1 WHERE id=?1 AND recipient=?2",
            params![ident, agent],
        )?;
        Ok(serde_json::from_str(&data)?)
    }
    pub fn queue_envelope(&self, peer: &str, envelope: &Envelope) -> Result<()> {
        self.connection.lock().unwrap().execute(
            "INSERT OR IGNORE INTO outbox(id,peer,data) VALUES(?1,?2,?3)",
            params![envelope.id, peer, serde_json::to_string(envelope)?],
        )?;
        Ok(())
    }
    pub fn outgoing(&self) -> Result<Vec<(String, Envelope, u64)>> {
        let c = self.connection.lock().unwrap();
        let mut s = c.prepare(
            "SELECT peer,data,attempts FROM outbox WHERE status='pending' AND due<=?1 LIMIT 20",
        )?;
        let rows = s.query_map([now()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
            ))
        })?;
        rows.map(|r| {
            let (p, d, a) = r?;
            Ok((p, serde_json::from_str(&d)?, a))
        })
        .collect()
    }
    pub fn outgoing_result(&self, ident: &str, success: bool, attempts: u64) -> Result<()> {
        self.connection.lock().unwrap().execute(
            "UPDATE outbox SET status=?2,attempts=attempts+1,due=?3 WHERE id=?1",
            params![
                ident,
                if success { "delivered" } else { "pending" },
                now() + ((2u64.saturating_pow(attempts.min(8) as u32)).min(300) as i64)
            ],
        )?;
        Ok(())
    }
    pub fn route(&self, ident: &str) -> Result<Option<Route>> {
        let c = self.connection.lock().unwrap();
        let d: Option<String> = c
            .query_row("SELECT data FROM routes WHERE id=?1", [ident], |r| r.get(0))
            .optional()?;
        d.map(|v| serde_json::from_str(&v).map_err(Into::into))
            .transpose()
    }
    pub fn register_route(&self, route: &Route) -> Result<()> {
        self.connection.lock().unwrap().execute("INSERT INTO routes(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![route.id,serde_json::to_string(route)?])?;
        Ok(())
    }
}
