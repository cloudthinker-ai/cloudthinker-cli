use cloudthinker_client::{AgentChatSelection, AgentView};

use super::{emit_json, terminal_text, write_line};

pub fn emit_agents(agents: &[AgentView], json: bool) -> Result<(), String> {
    if json {
        return emit_json(&agents);
    }
    let mut out = std::io::stdout().lock();
    if agents.is_empty() {
        return write_line(
            &mut out,
            "No agents in this workspace. Check your selected workspace with cloudthinker whoami.",
        );
    }
    write_line(&mut out, "AGENT ID\tALIAS\tTYPE\tACTIVE\tCHAT\tNAME")?;
    for agent in agents {
        write_line(
            &mut out,
            &format!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                agent.id,
                terminal_text(&agent.alias),
                agent.agent_type.label(),
                agent.is_active,
                agent.chat_selection.label(),
                terminal_text(&agent.name)
            ),
        )?;
    }
    write_line(
        &mut out,
        "Use an eligible custom agent UUID with chat --agent. Chat checks access and runtime requirements.",
    )?;
    write_line(
        &mut out,
        "The default agent is used when --agent is omitted; inactive custom agents cannot be selected.",
    )
}

pub fn emit_agent(agent: &AgentView, json: bool) -> Result<(), String> {
    if json {
        return emit_json(agent);
    }
    let mut out = std::io::stdout().lock();
    let lines = [
        format!("agent:       {}", agent.id),
        format!("alias:       {}", terminal_text(&agent.alias)),
        format!("name:        {}", terminal_text(&agent.name)),
        format!("type:        {}", agent.agent_type.label()),
        format!("active:      {}", agent.is_active),
        format!("chat:        {}", agent.chat_selection.label()),
        format!("description: {}", terminal_text(&agent.description)),
    ];
    for line in lines {
        write_line(&mut out, &line)?;
    }
    if agent.description_truncated {
        write_line(
            &mut out,
            "Description truncated; open Agents in the browser for more.",
        )?;
    }
    let hint = match agent.chat_selection {
        AgentChatSelection::Eligible => format!(
            "Select with chat --agent {}. Chat checks access and runtime requirements.",
            agent.id
        ),
        AgentChatSelection::Inactive => {
            "Activate this custom agent in the browser before using chat --agent.".into()
        }
        AgentChatSelection::Default => {
            "Use chat without --agent for the default agent. Explicit selection requires a custom agent."
                .into()
        }
    };
    write_line(&mut out, &hint)
}
