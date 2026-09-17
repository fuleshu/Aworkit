//! Python launch preparation preserves legacy argv and refuses weakened authority.
use aworkit_capability_host::{
    BuiltInProcessTools, HermeticProcessPort, HostToolLimitsV1, PythonInvocationV1,
    ToolAdapterError, ToolAuthorityModeV1,
};
use std::{collections::BTreeMap, time::Duration};

fn invocation() -> PythonInvocationV1 {
    PythonInvocationV1 {
        mode: ToolAuthorityModeV1::HostPython,
        interpreter: "configured-python".into(),
        script: "print('hello')".into(),
        arguments: vec!["literal argument".into()],
        working_directory: Some("workspace".into()),
        environment: BTreeMap::from([("EXPLICIT".into(), "value".into())]),
        limits: HostToolLimitsV1::default(),
    }
}

#[test]
fn python_preparation_preserves_frozen_interpreter_isolation_and_legacy_arguments() {
    let request = invocation();
    let spec = BuiltInProcessTools::<HermeticProcessPort>::python_spec(&request).unwrap();
    assert_eq!(spec.program, request.interpreter);
    assert_eq!(
        spec.arguments,
        ["-I", "-c", "print('hello')", "literal argument"]
    );
    assert_eq!(spec.working_directory, request.working_directory);
    assert_eq!(spec.environment, request.environment);
    assert_eq!(spec.timeout, request.limits.timeout);
}

#[test]
fn python_preparation_refuses_sandbox_downgrade_invalid_code_and_limits() {
    let mut request = invocation();
    request.mode = ToolAuthorityModeV1::SandboxedPython;
    assert!(matches!(
        BuiltInProcessTools::<HermeticProcessPort>::python_spec(&request),
        Err(ToolAdapterError::VerifiedIsolationUnavailable)
    ));
    request.mode = ToolAuthorityModeV1::HostShell;
    assert!(matches!(
        BuiltInProcessTools::<HermeticProcessPort>::python_spec(&request),
        Err(ToolAdapterError::AuthorityModeMismatch)
    ));
    request.mode = ToolAuthorityModeV1::HostPython;
    for script in [String::new(), "print(1)\0".into(), "x".repeat(262145)] {
        request.script = script;
        assert!(matches!(
            BuiltInProcessTools::<HermeticProcessPort>::python_spec(&request),
            Err(ToolAdapterError::InputBound)
        ));
    }
    request.script = "print(1)".into();
    for maximum in [0, 262145] {
        request.limits.maximum_output_bytes = maximum;
        assert!(BuiltInProcessTools::<HermeticProcessPort>::python_spec(&request).is_err());
    }
    request.limits = HostToolLimitsV1 {
        timeout: Duration::ZERO,
        ..Default::default()
    };
    assert!(BuiltInProcessTools::<HermeticProcessPort>::python_spec(&request).is_err());
}
