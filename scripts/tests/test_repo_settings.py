"""Offline contract tests: fake gh responses, never contact GitHub."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
MOCK = r'''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
method = args[args.index('--method') + 1] if '--method' in args else 'GET'
endpoint = next(a for a in args if a.startswith('repos/'))
with open(os.environ['MOCK_LOG'], 'a') as log:
    log.write(method + ' ' + endpoint + '\n')
root = Path(os.environ['MOCK_ROOT'])
scenario = os.environ['MOCK_SCENARIO']
repo = json.loads((root / '.github/settings/repository.json').read_text())
sec = json.loads((root / '.github/settings/security.json').read_text())
rule = json.loads((root / '.github/settings/rulesets/protect-integration-branches.json').read_text())
path = '/'.join(endpoint.split('/')[3:])
status = 200
body = {}
if method != 'GET':
    payload = json.loads(Path(args[args.index('--input') + 1]).read_text()) if '--input' in args else None
    with open(os.environ['MOCK_PAYLOADS'], 'a') as log:
        log.write(json.dumps({'method': method, 'path': path, 'payload': payload}) + '\n')
    if scenario == 'order-failure' and path == 'actions/permissions':
        sys.exit(1)
    sys.exit(0)
if path == '':
    body = {**repo['settings'], 'security_and_analysis': sec['security_and_analysis']}
    if scenario == 'drift': body['allow_squash_merge'] = not repo['settings']['allow_squash_merge']
    if scenario == 'denied': status = 403
elif path == 'topics': body = {'names': list(reversed(repo['topics']))}
elif path.startswith('rulesets?'):
    creating = scenario == 'created-classic' and 'POST ' + endpoint.split('?')[0] not in Path(os.environ['MOCK_LOG']).read_text()
    body = [[]] if scenario == 'order-missing' or creating else [[{'id': 7, 'name': rule['name'], 'source_type': 'Repository'}]]
elif path == 'rulesets/7':
    body = rule
    pr = next(r for r in body['rules'] if r['type'] == 'pull_request')
    pr['parameters'].update(require_extra_approval_for_unattributed_changes=True, required_reviewers=[])
    if scenario == 'rules-value': pr['parameters']['required_approving_review_count'] = 1
    if scenario == 'rules-missing-value': del pr['parameters']['require_last_push_approval']
    if scenario == 'rules-missing': body['rules'].remove(pr)
    if scenario == 'rules-extra': body['rules'].append({'type': 'creation'})
    if scenario == 'rules-duplicate': body['rules'].append(pr.copy())
    if scenario == 'rules-enforcement': body['enforcement'] = 'disabled'
    if scenario == 'rules-target': body['target'] = 'tag'
    if scenario == 'rules-branches': body['conditions']['ref_name']['include'].pop()
    if scenario == 'reordered': body['rules'].reverse()
    if scenario.startswith('order-'): body['enforcement'] = 'disabled'
    if scenario == 'unsafe': body['bypass_actors'] = [{'actor_id': 1, 'actor_type': 'Team', 'bypass_mode': 'always'}]
elif path == 'private-vulnerability-reporting':
    body = sec['private_vulnerability_reporting']
    if scenario.startswith('order-'): body['enabled'] = False
elif path == 'vulnerability-alerts':
    status = 404 if scenario == 'alerts-off' else 204
    body = None
elif path == 'automated-security-fixes': body = sec['automated_security_fixes']
elif path == 'code-scanning/default-setup':
    body = sec['codeql']
    if scenario == 'codeql-missing': status = 404
    if scenario in ('codeql-pending', 'codeql-pending-drift'): body['languages'] = []
    if scenario == 'codeql-pending-drift': body['query_suite'] = 'extended'
elif path == 'actions/permissions':
    body = {**sec['actions'], 'enabled': True, 'allowed_actions': 'selected'}
    if scenario.startswith('actions-'):
        _, enabled, policy = scenario.split('-')
        body.update(enabled=enabled == 'true', allowed_actions=policy, sha_pinning_required=False)
    if scenario.startswith('order-'): body['sha_pinning_required'] = False
elif path == 'actions/permissions/workflow': body = sec['workflow']
elif path == 'branches/main/protection':
    status = 200 if scenario in ('classic', 'created-classic', 'unsafe') or scenario.startswith('rules-') else 404
else:
    print('Unexpected endpoint: ' + endpoint, file=sys.stderr)
    sys.exit(3)
if '--include' in args:
    print('HTTP/2.0 ' + str(status))
    print('Content-Type: application/json\n')
if body is not None: print(json.dumps(body))
if status >= 400: sys.exit(1)
'''


class RepositorySettingsTests(unittest.TestCase):
    def run_script(self, scenario, *args, include_payloads=False, include_calls=False):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            fake = directory / 'gh'
            fake.write_text(MOCK)
            fake.chmod(0o755)
            log = directory / 'calls'
            payloads = directory / 'payloads'
            env = dict(os.environ, PATH=f'{directory}:{os.environ["PATH"]}',
                       MOCK_ROOT=str(ROOT), MOCK_LOG=str(log), MOCK_SCENARIO=scenario,
                       MOCK_PAYLOADS=str(payloads))
            result = subprocess.run([str(ROOT / 'scripts/repo-settings.sh'), *args],
                                    env=env, text=True, capture_output=True)
            calls = log.read_text().splitlines() if log.exists() else []
            writes = [call for call in calls if not call.startswith('GET ')]
            if include_payloads:
                return result, [json.loads(line) for line in payloads.read_text().splitlines()]
            return result, calls if include_calls else writes

    def test_matching_settings_are_read_only(self):
        result, writes = self.run_script('clean', 'check', 'example/copy')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('No drift: example/copy', result.stdout)
        self.assertEqual(writes, [])

    def test_drift_is_nonzero_and_read_only(self):
        result, writes = self.run_script('drift', 'check')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('allow_squash_merge', result.stdout)
        self.assertEqual(writes, [])

    def test_ruleset_server_defaults_and_rule_order_are_ignored(self):
        result, writes = self.run_script('reordered', 'check')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('No drift:', result.stdout)
        self.assertEqual(writes, [])

    def test_real_ruleset_drift_is_detected_and_blocks_protection_removal(self):
        for field in ('value', 'missing-value', 'missing', 'extra', 'duplicate',
                      'enforcement', 'target', 'branches'):
            with self.subTest(field=field):
                scenario = f'rules-{field}'
                result, writes = self.run_script(scenario, 'check')
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn('Drift: ruleset', result.stdout)
                self.assertEqual(writes, [])
                result, writes = self.run_script(scenario, 'apply', '--yes', '--remove-main-protection')
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn('keeping classic main protection', result.stderr)
                self.assertFalse(any(call.startswith('DELETE ') for call in writes))

    def test_apply_requires_yes_after_printing_diff(self):
        result, writes = self.run_script('drift', 'apply')
        self.assertEqual(result.returncode, 2)
        self.assertIn('Drift: repository', result.stdout)
        self.assertEqual(writes, [])

    def test_apply_updates_only_drift(self):
        result, writes = self.run_script('drift', 'apply', '--yes')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, ['PATCH repos/Folt-CLI/folt'])

    def test_actions_payload_preserves_enablement_and_allowed_actions(self):
        for enabled in (True, False):
            for policy in ('all', 'local_only', 'selected'):
                with self.subTest(enabled=enabled, policy=policy):
                    scenario = f'actions-{str(enabled).lower()}-{policy}'
                    result, writes = self.run_script(scenario, 'apply', '--yes', include_payloads=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(writes, [{
                        'method': 'PUT', 'path': 'actions/permissions',
                        'payload': {'enabled': enabled, 'allowed_actions': policy,
                                    'sha_pinning_required': True},
                    }])

    def test_ruleset_is_last_write_for_creation_and_update(self):
        for scenario, request in (('order-missing', 'POST repos/Folt-CLI/folt/rulesets'),
                                  ('order-existing', 'PUT repos/Folt-CLI/folt/rulesets/7')):
            with self.subTest(scenario=scenario):
                result, writes = self.run_script(scenario, 'apply', '--yes')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('PUT repos/Folt-CLI/folt/private-vulnerability-reporting', writes)
                self.assertIn('PUT repos/Folt-CLI/folt/actions/permissions', writes)
                self.assertEqual(writes[-1], request)

    def test_failed_setting_write_does_not_apply_ruleset(self):
        result, writes = self.run_script('order-failure', 'apply', '--yes')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('PUT repos/Folt-CLI/folt/actions/permissions', writes)
        self.assertFalse(any('/rulesets' in call for call in writes))

    def test_permission_failure_aborts_without_writes(self):
        result, writes = self.run_script('denied', 'apply', '--yes')
        self.assertEqual(result.returncode, 2)
        self.assertIn('HTTP 403', result.stderr)
        self.assertEqual(writes, [])

    def test_alerts_404_means_disabled(self):
        result, writes = self.run_script('alerts-off', 'apply', '--yes')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, ['PUT repos/Folt-CLI/folt/vulnerability-alerts'])

    def test_missing_codeql_is_configured(self):
        result, writes = self.run_script('codeql-missing', 'apply', '--yes', include_payloads=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, [{
            'method': 'PATCH', 'path': 'code-scanning/default-setup',
            'payload': {'state': 'configured', 'languages': ['actions'],
                        'query_suite': 'default'},
        }])

    def test_codeql_provisioning_is_pending_and_does_not_reconfigure(self):
        for mode in ('check', 'apply'):
            with self.subTest(mode=mode):
                args = (mode, '--yes') if mode == 'apply' else (mode,)
                result, writes = self.run_script('codeql-pending', *args)
                self.assertEqual(result.returncode, 1 if mode == 'check' else 0, result.stderr)
                self.assertIn('Pending: CodeQL is configured but languages are empty', result.stdout)
                self.assertNotIn('Drift: codeql', result.stdout)
                self.assertNotIn('No drift:', result.stdout)
                self.assertEqual(writes, [])

    def test_codeql_other_drift_is_not_hidden_by_empty_languages(self):
        result, writes = self.run_script('codeql-pending-drift', 'check')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('Drift: codeql', result.stdout)
        self.assertNotIn('Pending:', result.stdout)
        self.assertEqual(writes, [])

    def test_classic_protection_requires_explicit_step(self):
        result, writes = self.run_script('classic', 'apply', '--yes')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, [])
        result, writes = self.run_script('classic', 'apply', '--yes', '--remove-main-protection')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, ['DELETE repos/Folt-CLI/folt/branches/main/protection'])

    def test_new_ruleset_with_server_defaults_allows_protection_removal(self):
        result, writes = self.run_script('created-classic', 'apply', '--yes', '--remove-main-protection')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, [
            'POST repos/Folt-CLI/folt/rulesets',
            'DELETE repos/Folt-CLI/folt/branches/main/protection',
        ])

    def test_unverified_ruleset_cannot_remove_classic_protection(self):
        result, writes = self.run_script('unsafe', 'apply', '--yes', '--remove-main-protection')
        self.assertEqual(result.returncode, 2)
        self.assertIn('keeping classic main protection', result.stderr)
        self.assertFalse(any(call.startswith('DELETE ') for call in writes))


if __name__ == '__main__':
    unittest.main()
