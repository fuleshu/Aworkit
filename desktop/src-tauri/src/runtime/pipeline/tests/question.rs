//! Behavioral checks for the two model-facing question capabilities.
//!
//! A question suspends the Run through the same durable challenge an approval
//! uses, so these tests cross the real broker, the record store and the resume
//! path: the answer must be delivered exactly once, must never be invented, and
//! a cancellation must reach the model as an ordinary result.
use super::*;
use crate::runtime::approvals::ApprovalMode;
use crate::runtime::tool_loop::question::{QuestionAnswerV1, QuestionKindV1};
use crate::runtime::tool_loop::{ASK_USER_CAPABILITY_ID, BROWSE_CAPABILITY_ID};

fn question_request(
    pipeline: &WorkflowExecutionPipeline,
    metadata: CredentialMetadataV1,
    project: &Path,
    capability_id: &str,
) -> WorkflowExecutionRequestV1 {
    let mut request = tool_bound_request(pipeline, metadata, project, &[capability_id]);
    // The question is answered by the user, so no approval mode may stand in
    // for the answer: the fixture asks exactly like a real Chat does.
    request.approvals.mode = ApprovalMode::AskForApproval;
    request
}

fn project(root: &TempDir) -> std::path::PathBuf {
    let project = root.path().join("project");
    fs::create_dir(&project).expect("project");
    fs::create_dir(project.join(".git")).expect("git metadata");
    fs::write(project.join(".git/HEAD"), b"ref: refs/heads/main\n").expect("Git HEAD");
    project
}

#[test]
fn an_ask_user_call_suspends_the_run_with_a_typed_question() {
    let root = TempDir::new().expect("root");
    let project = project(&root);
    let (pipeline, _store, metadata, calls, observed) =
        setup_tool_pipeline(&root, ToolScriptV1::Question);
    let request = question_request(&pipeline, metadata, &project, ASK_USER_CAPABILITY_ID);
    let chat_id = request.chat_id.clone();
    let run_id = request.run_id.clone();

    let suspended = pipeline.execute(request.clone()).expect("question execution");
    assert_eq!(
        suspended.status,
        WorkflowExecutionStatusV1::AwaitingAnswer,
        "{:?}",
        suspended.error
    );
    // The suspension is a question, not an authority decision.
    let suspension = suspended.approval.expect("suspension payload");
    let question = suspension.question.expect("question payload");
    assert_eq!(question.kind, QuestionKindV1::Choice);
    assert_eq!(
        question.prompt,
        "Which release channel should this build target?"
    );
    assert_eq!(question.options.len(), 2);
    assert!(question.allow_free_text);
    assert!(suspension.filesystem.is_none());
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the question reaches the user before any further model turn"
    );
    assert!(observed.lock().expect("results").is_empty());
    // Nothing was asked twice: the pass is parked on the pending question.
    assert_eq!(
        pipeline
            .validate_question_target(
                &suspension.decision_id,
                chat_id.as_str(),
                &QuestionAnswerV1 {
                    option_id: Some("stable".into()),
                    ..QuestionAnswerV1::default()
                },
            )
            .is_ok(),
        true
    );
    assert!(
        pipeline
            .validate_question_target("question.unknown", chat_id.as_str(), &QuestionAnswerV1::cancelled())
            .is_err(),
        "an unknown question id is refused"
    );
    assert!(
        pipeline
            .validate_question_target(
                &suspension.decision_id,
                "chat.somewhere-else",
                &QuestionAnswerV1::cancelled()
            )
            .is_err(),
        "a question is owner-scoped to its Chat"
    );
    assert!(
        pipeline
            .validate_question_target(
                &suspension.decision_id,
                chat_id.as_str(),
                &QuestionAnswerV1 {
                    option_id: Some("not-offered".into()),
                    ..QuestionAnswerV1::default()
                }
            )
            .is_err(),
        "only an offered option is accepted"
    );
    assert_eq!(
        pipeline
            .subagent_catalog(chat_id.as_str(), &run_id)
            .expect("catalog")
            .len(),
        0
    );
}

#[test]
fn a_delivered_answer_reaches_the_model_once_and_resumes_the_same_pass() {
    let root = TempDir::new().expect("root");
    let project = project(&root);
    let (pipeline, _store, metadata, calls, observed) =
        setup_tool_pipeline(&root, ToolScriptV1::Question);
    let request = question_request(&pipeline, metadata, &project, ASK_USER_CAPABILITY_ID);
    let chat_id = request.chat_id.clone();

    let suspended = pipeline.execute(request).expect("question execution");
    let question_id = suspended
        .approval
        .as_ref()
        .expect("suspension payload")
        .decision_id
        .clone();
    let answer = QuestionAnswerV1 {
        option_id: Some("beta".into()),
        free_text: Some("start with beta".into()),
        ..QuestionAnswerV1::default()
    };
    let resumed = pipeline
        .resume_question(&question_id, chat_id.as_str(), &answer)
        .expect("resume");
    assert_eq!(
        resumed.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        resumed.error
    );
    assert_eq!(resumed.assistant_text.as_deref(), Some("tool loop complete"));
    // The model saw the answer as an ordinary tool result.
    {
        let results = observed.lock().expect("results");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["optionId"], "beta");
        assert_eq!(results[0]["freeText"], "start with beta");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one question, one final turn");

    // Consuming the answer twice is a replay, not a second delivery.
    observed.lock().expect("results").clear();
    let replayed = pipeline
        .resume_question(&question_id, chat_id.as_str(), &answer)
        .expect("replayed resume");
    assert_eq!(
        replayed.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        replayed.error
    );
    assert!(
        observed.lock().expect("results").is_empty(),
        "a replayed resume never asks the model again"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // A different answer for an already answered question is refused.
    assert!(
        pipeline
            .resume_question(
                &question_id,
                chat_id.as_str(),
                &QuestionAnswerV1 {
                    option_id: Some("stable".into()),
                    ..QuestionAnswerV1::default()
                }
            )
            .is_err(),
        "an answered question cannot be answered again"
    );
}

#[test]
fn a_cancelled_question_is_an_ordinary_result_the_model_continues_from() {
    let root = TempDir::new().expect("root");
    let project = project(&root);
    let (pipeline, _store, metadata, calls, observed) =
        setup_tool_pipeline(&root, ToolScriptV1::Browse);
    let request = question_request(&pipeline, metadata, &project, BROWSE_CAPABILITY_ID);
    let chat_id = request.chat_id.clone();

    let suspended = pipeline.execute(request).expect("browse execution");
    assert_eq!(
        suspended.status,
        WorkflowExecutionStatusV1::AwaitingAnswer,
        "{:?}",
        suspended.error
    );
    let suspension = suspended.approval.expect("suspension payload");
    let question = suspension.question.as_ref().expect("question payload");
    assert_eq!(question.kind, QuestionKindV1::Folder);
    assert!(question.options.is_empty());

    // Dismissing the operating-system dialog is `cancelled`, never an error.
    let resumed = pipeline
        .resume_question(
            &suspension.decision_id,
            chat_id.as_str(),
            &QuestionAnswerV1::cancelled(),
        )
        .expect("cancelled resume");
    assert_eq!(
        resumed.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        resumed.error
    );
    let observed = observed.lock().expect("results");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0]["cancelled"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_chosen_folder_is_delivered_as_the_answer_value() {
    let root = TempDir::new().expect("root");
    let project = project(&root);
    let (pipeline, _store, metadata, _calls, observed) =
        setup_tool_pipeline(&root, ToolScriptV1::Browse);
    let request = question_request(&pipeline, metadata, &project, BROWSE_CAPABILITY_ID);
    let chat_id = request.chat_id.clone();

    let suspended = pipeline.execute(request).expect("browse execution");
    let question_id = suspended
        .approval
        .as_ref()
        .expect("suspension payload")
        .decision_id
        .clone();
    // A path question is answered with a path and nothing else.
    assert!(
        pipeline
            .resume_question(
                &question_id,
                chat_id.as_str(),
                &QuestionAnswerV1 {
                    free_text: Some("/tmp".into()),
                    ..QuestionAnswerV1::default()
                }
            )
            .is_err(),
        "a folder question does not accept free text"
    );
    let resumed = pipeline
        .resume_question(
            &question_id,
            chat_id.as_str(),
            &QuestionAnswerV1 {
                path: Some("/home/user/reports".into()),
                ..QuestionAnswerV1::default()
            },
        )
        .expect("path resume");
    assert_eq!(
        resumed.status,
        WorkflowExecutionStatusV1::Succeeded,
        "{:?}",
        resumed.error
    );
    let observed = observed.lock().expect("results");
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0]["path"], "/home/user/reports");
}
