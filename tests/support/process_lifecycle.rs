// SPDX-License-Identifier: MPL-2.0
//! Shared real-process fixture for adapter and persisted-runtime lifecycle tests.

use std::{fs, path::Path};

/// Writes a schema-constrained APP package into `dir`. Modes selected by input
/// exercise terminal framing and live boundaries; child readiness uses a pipe.
/// The fixture writes numeric probe files only and never writes delivered secrets.
pub fn write_plugin(dir: &Path) {
    fs::create_dir_all(dir.join("schemas")).expect("schemas");
    fs::write(dir.join("schemas/input.json"), "{}").expect("input");
    fs::write(
        dir.join("schemas/output.json"),
        r#"{"type":"object","required":["stdout"],"properties":{"stdout":{"type":"string"}}}"#,
    )
    .expect("output");
    fs::write(
        dir.join("plugin.yaml"),
        r#"
schema: transist.plugin.v1
id: fixture.lifecycle
version: 1.0.0
adapter: process
entrypoint:
  type: process
  command: plugin.py
package:
  files: [plugin.py, schemas/input.json, schemas/output.json]
actions:
  - name: invoke
    input_schema: schemas/input.json
    output_schema: schemas/output.json
    secret_inputs:
      - name: LIFECYCLE_SECRET
        required: false
        binding:
          type: env
          name: FIXTURE_SECRET
    bindings: {}
capabilities:
  supports_cancel: true
  supports_health: false
  supports_artifact_hints: true
  supports_evidence_hints: true
"#,
    )
    .expect("manifest");
    fs::write(dir.join("plugin.py"), r#"#!/usr/bin/env python3
import base64, json, os, signal, sys, time

def emit(value):
    print(json.dumps(value), flush=True)

json.loads(sys.stdin.readline())
emit({'type':'app.handshake.response.v1','protocol_version':'app.v1',
      'plugin':{'id':'fixture.lifecycle','version':'1.0.0'},'adapter_kind':'process',
      'capabilities':{'actions':['invoke'],'supports_cancel':True,'supports_health':False,
                      'supports_artifact_hints':True,'supports_evidence_hints':True}})
line = sys.stdin.readline()
if not line:
    sys.exit(0)
request = json.loads(line)
mode = request['input'].get('mode', 'leader')
signal.signal(signal.SIGTERM, signal.SIG_IGN)
root = request['paths']['workspace_dir']
if mode in ('descendant', 'failed_descendant'):
    read_fd, write_fd = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(read_fd)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        os.write(write_fd, b'1')
        os.close(write_fd)
        time.sleep(20)
        os._exit(0)
    os.close(write_fd)
    os.read(read_fd, 1)
    os.close(read_fd)
    with open(os.path.join(root, 'lifecycle-child.pid'), 'w') as f:
        f.write(str(child))
with open(os.path.join(root, 'lifecycle-leader.pid'), 'w') as f:
    f.write(str(os.getpid()))
secret = os.environ.get('FIXTURE_SECRET', '')
text = 'terminal-output' if not secret else ' '.join([
    secret, base64.b64encode(secret.encode()).decode(),
    base64.urlsafe_b64encode(secret.encode()).decode().rstrip('=')])
failed = mode in ('failed_descendant', 'failed_empty', 'failed_empty_clean')
result = {'type':'app.invoke.result.v1','protocol_version':'app.v1',
          'invoke_id':request['invoke_id'],'status':'failed' if failed else 'success',
          'output':{'stdout':text},
          'artifact_hints':[{'name':'terminal.txt','contents':text}],
          'evidence_hints':[{'name':'terminal-trace.txt','contents':text}],
          'metrics':{}, 'usage':None,
          'error':{'code':'tool_execution_error','class':'tool','retryability':'safe','message':text} if failed else None}
if mode in ('invalid_output', 'invalid_output_clean', 'failed_empty', 'failed_empty_clean'):
    result['output'] = {}
if mode in ('invalid_identity', 'invalid_identity_clean'):
    result['invoke_id'] = 'wrong'
if mode in ('invalid_version', 'invalid_version_clean'):
    result['protocol_version'] = 'app.v999'
if mode in ('invalid_type', 'invalid_type_clean'):
    result['type'] = 'invalid.result'
if mode == 'pre_terminal':
    time.sleep(20)
emit(result)
with open(os.path.join(root, 'lifecycle-terminal.ready'), 'w') as f:
    f.write('ready')
if mode not in ('clean', 'descendant', 'failed_descendant', 'failed_empty_clean',
                'invalid_output_clean', 'invalid_identity_clean', 'invalid_version_clean', 'invalid_type_clean'):
    time.sleep(20)
"#).expect("script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.join("plugin.py"), fs::Permissions::from_mode(0o755))
            .expect("executable");
    }
}
