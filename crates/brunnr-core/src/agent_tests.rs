// SPDX-License-Identifier: Apache-2.0

use futures_util::{future::BoxFuture, stream, FutureExt, StreamExt};

use super::*;

#[derive(Debug)]
struct MockAgent;

impl Agent for MockAgent {
    fn spawn(&self, request: SpawnRequest) -> BoxFuture<'_, AgentResult<AgentSession>> {
        async move {
            Ok(AgentSession {
                id: "session-1".to_string(),
                role: request.role,
                agent: request.agent,
            })
        }
        .boxed()
    }

    fn send(
        &self,
        session: &AgentSession,
        message: AgentMessage,
    ) -> BoxFuture<'_, AgentResult<AgentResponse>> {
        let response = format!("{}:{}", session.agent, message.content);
        async move { Ok(AgentResponse { content: response }) }.boxed()
    }

    fn stream(
        &self,
        _session: &AgentSession,
        message: AgentMessage,
    ) -> BoxFuture<'_, AgentResult<AgentEventStream>> {
        async move {
            let events = vec![Ok(AgentEvent::Text(message.content)), Ok(AgentEvent::Done)];
            Ok(Box::pin(stream::iter(events)) as AgentEventStream)
        }
        .boxed()
    }

    fn capabilities(&self) -> AgentCapabilities {
        AgentCapabilities {
            streaming: true,
            tools: true,
            mcp: true,
        }
    }
}

#[tokio::test]
async fn mock_agent_satisfies_spawn_send_stream_contract() {
    let agent = MockAgent;
    let session = agent
        .spawn(SpawnRequest {
            role: Role::Worker,
            agent: "codex".to_string(),
            model: None,
            working_dir: None,
        })
        .await
        .expect("spawn should succeed");

    let response = agent
        .send(
            &session,
            AgentMessage {
                content: "hello".to_string(),
            },
        )
        .await
        .expect("send should succeed");

    let events = agent
        .stream(
            &session,
            AgentMessage {
                content: "hello".to_string(),
            },
        )
        .await
        .expect("stream should open")
        .collect::<Vec<_>>()
        .await;

    assert_eq!(
        response,
        AgentResponse {
            content: "codex:hello".to_string()
        }
    );
    assert_eq!(
        events
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("events should be ok"),
        vec![AgentEvent::Text("hello".to_string()), AgentEvent::Done]
    );
    assert_eq!(
        agent.capabilities(),
        AgentCapabilities {
            streaming: true,
            tools: true,
            mcp: true,
        }
    );
}
