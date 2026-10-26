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
    if scenario == 'drift': body['allow_squash_merge'] = True
    if scenario == 'denied': status = 403
elif path == 'topics': body = {'names': list(reversed(repo['topics']))}
elif path.startswith('rulesets?'):
    body = [[]] if scenario == 'order-missing' else [[{'id': 7, 'name': rule['name'], 'source_type': 'Repository'}]]
elif path == 'rulesets/7':
    body = rule
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
elif path == 'actions/permissions':
    body = {**sec['actions'], 'enabled': True, 'allowed_actions': 'selected'}
    if scenario.startswith('actions-'):
        _, enabled, policy = scenario.split('-')
        body.update(enabled=enabled == 'true', allowed_actions=policy, sha_pinning_required=False)
    if scenario.startswith('order-'): body['sha_pinning_required'] = False
elif path == 'actions/permissions/workflow': body = sec['workflow']
elif path.startswith('labels?'):
    body = [json.loads((root / '.github/labels.json').read_text()) + [{'name': 'Keep me', 'color': '000000', 'description': ''}]]
    if scenario == 'labels' or scenario.startswith('order-'):
        body[0].pop(0)
        body[0][0]['color'] = 'ffffff'
elif path == 'branches/main/protection':
    status = 200 if scenario in ('classic', 'unsafe') else 404
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
    def run_script(self, scenario, *args, include_payloads=False):
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
            return result, writes

    def test_matching_settings_and_extra_labels_are_read_only(self):
        result, writes = self.run_script('clean', 'check', 'example/copy')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('No drift: example/copy', result.stdout)
        self.assertEqual(writes, [])

    def test_drift_is_nonzero_and_read_only(self):
        result, writes = self.run_script('drift', 'check')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn('allow_squash_merge', result.stdout)
        self.assertEqual(writes, [])

    def test_apply_requires_yes_after_printing_diff(self):
        result, writes = self.run_script('drift', 'apply')
        self.assertEqual(result.returncode, 2)
        self.assertIn('Drift: repository', result.stdout)
        self.assertEqual(writes, [])

    def test_apply_updates_only_drift(self):
        result, writes = self.run_script('drift', 'apply', '--yes')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, ['PATCH repos/Bojan-Ivanovski/folt'])

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
        for scenario, request in (('order-missing', 'POST repos/Bojan-Ivanovski/folt/rulesets'),
                                  ('order-existing', 'PUT repos/Bojan-Ivanovski/folt/rulesets/7')):
            with self.subTest(scenario=scenario):
                result, writes = self.run_script(scenario, 'apply', '--yes')
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('PUT repos/Bojan-Ivanovski/folt/private-vulnerability-reporting', writes)
                self.assertIn('PUT repos/Bojan-Ivanovski/folt/actions/permissions', writes)
                self.assertTrue(any('/labels' in call for call in writes))
                self.assertEqual(writes[-1], request)

    def test_failed_setting_write_does_not_apply_ruleset(self):
        result, writes = self.run_script('order-failure', 'apply', '--yes')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('PUT repos/Bojan-Ivanovski/folt/actions/permissions', writes)
        self.assertFalse(any('/rulesets' in call for call in writes))

    def test_permission_failure_aborts_without_writes(self):
        result, writes = self.run_script('denied', 'apply', '--yes')
        self.assertEqual(result.returncode, 2)
        self.assertIn('HTTP 403', result.stderr)
        self.assertEqual(writes, [])

    def test_alerts_404_means_disabled(self):
        result, writes = self.run_script('alerts-off', 'apply', '--yes')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, ['PUT repos/Bojan-Ivanovski/folt/vulnerability-alerts'])

    def test_missing_codeql_is_configured(self):
        result, writes = self.run_script('codeql-missing', 'apply', '--yes', include_payloads=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, [{
            'method': 'PATCH', 'path': 'code-scanning/default-setup',
            'payload': {'state': 'configured', 'languages': ['actions'],
                        'query_suite': 'default'},
        }])

    def test_labels_created_updated_and_never_deleted(self):
        result, writes = self.run_script('labels', 'apply', '--yes')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(writes), 2)
        self.assertTrue(writes[0].startswith('POST ') and writes[1].startswith('PATCH '))

    def test_classic_protection_requires_explicit_step(self):
        result, writes = self.run_script('classic', 'apply', '--yes')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, [])
        result, writes = self.run_script('classic', 'apply', '--yes', '--remove-main-protection')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(writes, ['DELETE repos/Bojan-Ivanovski/folt/branches/main/protection'])

    def test_unverified_ruleset_cannot_remove_classic_protection(self):
        result, writes = self.run_script('unsafe', 'apply', '--yes', '--remove-main-protection')
        self.assertEqual(result.returncode, 2)
        self.assertIn('keeping classic main protection', result.stderr)
        self.assertFalse(any(call.startswith('DELETE ') for call in writes))


if __name__ == '__main__':
    unittest.main()
