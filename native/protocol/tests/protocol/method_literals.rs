use std::collections::HashSet;

use fluxdown_protocol::method::{AGENT_CAPABILITIES, CLIENT_CAPABILITIES, DAEMON_CAPABILITIES};

#[test]
fn canonical_capability_literals_are_exact_and_unique() {
    let daemon = &[
        "daemon.tasks",
        "daemon.queues",
        "daemon.groups",
        "daemon.config",
        "daemon.rss",
        "daemon.plugins",
        "daemon.components",
        "daemon.webhooks",
        "daemon.selections",
        "daemon.files",
    ];
    let agent = &[
        "agent.gateway",
        "agent.auth",
        "agent.sync",
        "agent.remoteTasks",
        "agent.billing",
        "agent.referrals",
        "agent.deviceLink",
        "agent.externalCapture",
        "agent.openTaskFiles",
        "agent.copyTaskFiles",
    ];
    let client = &["client.selections"];
    assert_eq!(DAEMON_CAPABILITIES, daemon);
    assert_eq!(AGENT_CAPABILITIES, agent);
    assert_eq!(CLIENT_CAPABILITIES, client);
    let all = daemon
        .iter()
        .chain(agent)
        .chain(client)
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(all.iter().copied().collect::<HashSet<_>>().len(), all.len());
}
