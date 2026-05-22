use spur_acp::types::SessionId;
use spur_pm::Issue;

use crate::orchestrator::util::enforce_log_cap;
use crate::orchestrator::Orchestrator;

impl Orchestrator {
    pub(super) fn build_brain_prompt(
        &self,
        task: &str,
        issue: Option<&Issue>,
        session_id: &SessionId,
        brain_name: &str,
    ) -> String {
        if self.config.brain.delegation.framework == "v1" {
            self.build_brain_prompt_v1(task, issue, session_id, brain_name)
        } else {
            self.build_brain_prompt_legacy(task, issue)
        }
    }

    pub(super) fn build_brain_prompt_legacy(&self, task: &str, issue: Option<&Issue>) -> String {
        let mut prompt = String::new();

        // System instructions.
        prompt.push_str(
            "You are coordinating a coding task. You have two kinds of tools:\n\
             \n\
             1. Your own tools (filesystem, bash, git) — use these to investigate and code directly.\n\
             2. SPUR delegation tools — use these to hand work to specialized worker agents.\n\
             \n\
             When to delegate vs do it yourself:\n\
             - Delegate when subtasks are INDEPENDENT and can run in parallel\n\
             - Delegate to match agent strengths\n\
             - Do it yourself for quick tasks or when you need tight iterative control\n\
             - Always review worker output before approving\n\n",
        );

        // Issue context.
        if let Some(issue) = issue {
            prompt.push_str(&format!(
                "## Issue #{}: {}\n\n{}\n\nLabels: {}\nStatus: {}\n\n",
                issue.id,
                issue.title,
                issue.body,
                issue.labels.join(", "),
                issue.status,
            ));
        }

        // Project-specific context.
        if let Some(ref append) = self.config.brain.prompt.append {
            prompt.push_str(&format!("## Project Context\n\n{}\n\n", append));
        }

        prompt.push_str(Self::notebook_availability_prompt());

        // Task.
        prompt.push_str(&format!("## Task\n\n{}\n", task));

        prompt
    }

    pub(super) fn build_brain_prompt_v1(
        &self,
        task: &str,
        issue: Option<&Issue>,
        session_id: &SessionId,
        brain_name: &str,
    ) -> String {
        let mut prompt = String::new();
        prompt.push_str(&self.render_header());
        prompt.push_str(&self.render_workers_block());
        if let Some(framework) = crate::skills::load_skill("brain-delegation", &self.repo_root) {
            prompt.push_str(&framework);
        }
        let agent_skill = format!("brain-delegation-{}", brain_name);
        if let Some(guidance) = crate::skills::load_skill(&agent_skill, &self.repo_root) {
            prompt.push_str(&guidance);
        }
        prompt.push_str(Self::notebook_availability_prompt());
        self.append_issue_and_task(&mut prompt, task, issue);
        self.log_prompt_once(&prompt, session_id);
        prompt
    }

    pub(super) fn render_header(&self) -> String {
        "You are a brain coordinating a coding task. You have two kinds of tools:\n\
         \n\
         1. Your own tools (filesystem, bash, git) — for investigation and direct edits.\n\
         2. SPUR delegation tools (delegate_to_worker, delegate_parallel, list_available_workers) — for handing work to worker agents that run in isolated worktrees.\n\n".into()
    }

    pub(super) fn render_workers_block(&self) -> String {
        let mut out = String::from("## Available worker agents\n\n");
        let mut agents: Vec<_> = self.registry.worker_capable().into_iter().collect();
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        let mut any_listed = false;
        for agent in agents {
            if agent.delegation.good_for.is_empty() {
                continue;
            }
            any_listed = true;
            let tier = agent
                .delegation
                .tier
                .map(|t| match t {
                    spur_acp::config::Tier::Specialist => "specialist",
                    spur_acp::config::Tier::Generalist => "generalist",
                })
                .unwrap_or("generalist");
            let cost = format!("{:?}", agent.cost_tier).to_lowercase();
            let desc = agent
                .delegation
                .description
                .as_deref()
                .unwrap_or("(no description)");
            out.push_str(&format!(
                "### {}  ({}, cost: {})\n{}\n\n",
                agent.name, tier, cost, desc,
            ));
        }
        if !any_listed {
            out.push_str("(no worker-capable agents with descriptors configured)\n\n");
        }
        out
    }

    pub(super) fn append_issue_and_task(
        &self,
        prompt: &mut String,
        task: &str,
        issue: Option<&Issue>,
    ) {
        // Issue context.
        if let Some(issue) = issue {
            prompt.push_str(&format!(
                "## Issue #{}: {}\n\n{}\n\nLabels: {}\nStatus: {}\n\n",
                issue.id,
                issue.title,
                issue.body,
                issue.labels.join(", "),
                issue.status,
            ));
        }

        // Project-specific context.
        if let Some(ref append) = self.config.brain.prompt.append {
            prompt.push_str(&format!("## Project Context\n\n{}\n\n", append));
        }

        // Task.
        prompt.push_str(&format!("## Task\n\n{}\n", task));
    }

    fn notebook_availability_prompt() -> &'static str {
        "## NOTEBOOK AVAILABILITY\n\n\
         The `notebook.*` MCP server is always reachable, but a notebook may not be loaded. \
         If a notebook tool returns `notebook_not_open`, tell the user in chat: \
         \"I need a notebook open to do that - try `/notebook <path>` or `/notebook new`.\"\n\n"
    }

    pub(super) fn log_prompt_once(&self, prompt: &str, session_id: &SessionId) {
        let dir = self.repo_root.join(".spur/logs/brain-prompts");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::debug!(error = %e, "could not create brain-prompts log dir");
            return;
        }
        // Use the spur session id as the filename so that repeated calls within
        // the same session overwrite the prior log (one log per session intent).
        // SessionId wraps a UUID string, which is filename-safe by construction.
        let path = dir.join(format!("{}.md", session_id));
        if let Err(e) = std::fs::write(&path, prompt) {
            tracing::debug!(error = %e, path = %path.display(), "could not write prompt log");
        }
        enforce_log_cap(&dir, 50 * 1024 * 1024);
    }
}
