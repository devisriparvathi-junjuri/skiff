import hashlib
import unittest
import tempfile
from pathlib import Path
import check


class BoundaryTests(unittest.TestCase):
    def test_rust_aliases_grouped_imports_reexports_and_fully_qualified_io(self):
        for source in [
            "use skiff_core as core; core::git::diff();",
            "use skiff_core::{config as c, git}; c::load();",
            "pub use skiff_client::ledger as rows;",
            "std::fs::read(path);", "use std::{fs as disk}; disk::read(path);",
            "tokio::process::Command::new(p);", "use std::env as e; e::var(x);",
            "use crate::config as c; c::list_projects();",
            "include!(\"local_owner.rs\");",
            "use std::path::Path; Path::new(p).exists();",
        ]:
            with self.subTest(source=source): self.assertTrue(check.risks(source, ".rs"))

    def test_typescript_aliases_reexports_dynamic_and_direct_io(self):
        for source in [
            'import { invoke as send } from "@tauri-apps/api/core"; send("list_projects");',
            'export { readFile } from "@tauri-apps/plugin-fs";',
            'const disk = await import("node:fs");',
            'const disk = await import(`@tauri-apps/plugin-fs`);',
            'import * as shell from "@tauri-apps/plugin-shell";',
            'globalThis.fetch(url);', 'new WebSocket(url);', 'require("fs").readFileSync(p);',
        ]:
            with self.subTest(source=source): self.assertTrue(check.risks(source, ".ts"))

    def test_comments_strings_lifetimes_and_contract_imports(self):
        self.assertEqual(check.risks('// std::fs\n/* outer /* config */ */ fn f(x: &\'static str) {}', '.rs'), [])
        self.assertEqual(check.risks('const X: &str = r#"std::fs"#; use skiff_host_api::Hello;', '.rs'), [])
        self.assertEqual(check.risks('use skiff_client::host::HostConnection;', '.rs'), [])

    def test_exact_baseline_cannot_admit_new_access_using_an_existing_alias(self):
        source = 'use skiff_core::config as c; fn old() { c::load(); }'
        entry = lambda s: {'sha256': hashlib.sha256(s.encode()).hexdigest(), 'reasons': check.risks(s, '.rs')}
        baseline = {'old.rs': entry(source)}
        self.assertEqual(check.compare(baseline, baseline), [])
        self.assertTrue(check.compare({'old.rs': entry(source + ' fn new() { c::add_project(); }')}, baseline))
        self.assertTrue(check.compare({'new.rs': entry(source)}, baseline))
        self.assertTrue(check.compare({}, baseline))

    def fixture(self, root, name, source):
        file = root / name
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(source)

    def desktop_fixture(self, root):
        self.fixture(root, "apps/desktop/package.json", "{}")
        self.fixture(root, "apps/desktop/src-tauri/Cargo.toml", "")
        self.fixture(root, "apps/desktop/src-tauri/src/commands.rs",
                     "pub async fn read_image(p: PathBuf) { tokio::fs::read(p).await; }")
        self.fixture(root, "apps/desktop/src/platform/os.ts",
                     'import { invoke } from "@tauri-apps/api/core"; export const openPath = invoke;')

    def assert_new_caller_rejected(self, root, baseline, name, source):
        self.fixture(root, name, source)
        actual = check.inventory(root)
        self.assertIn(name, actual)
        self.assertTrue(any(error.startswith(name + ":") for error in check.compare(actual, baseline)))
        # The existing adapter's bytes stay unchanged; its exemption cannot
        # silently exempt new callers or re-export bridges.
        for adapter in ("apps/desktop/src/platform/os.ts", "apps/desktop/src-tauri/src/commands.rs"):
            self.assertEqual(actual[adapter]["sha256"], baseline[adapter]["sha256"])

    def test_inventory_rejects_existing_rust_adapter_and_reexport_callers(self):
        for bridge, caller in [
            (None, 'pub(crate) async fn preview(path: std::path::PathBuf) -> Result<tauri::ipc::Response, String> { crate::commands::read_image(path).await }'),
            ('pub use crate::commands::read_image as preview;', 'use crate::bridge::preview; fn load() { preview(path); }'),
            ('pub use crate::{commands as local};', 'crate::bridge::local::read_image(path);'),
        ]:
            with self.subTest(caller=caller), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                self.desktop_fixture(root)
                if bridge:
                    self.fixture(root, "apps/desktop/src-tauri/src/bridge.rs", bridge)
                baseline = check.inventory(root)
                self.assert_new_caller_rejected(root, baseline, "apps/desktop/src-tauri/src/preview.rs", caller)

    def test_rust_root_and_inline_module_reexports_are_providers(self):
        for bridge, caller in [
            ('pub use crate::commands::read_image as preview;', 'crate::preview(path);'),
            ('pub mod adapter { pub use crate::commands::read_image; }', 'crate::adapter::read_image(path);'),
        ]:
            with self.subTest(bridge=bridge), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                self.desktop_fixture(root)
                self.fixture(root, "apps/desktop/src-tauri/src/lib.rs", bridge)
                baseline = check.inventory(root)
                self.assert_new_caller_rejected(root, baseline, "apps/desktop/src-tauri/src/preview.rs", caller)

    def test_inventory_rejects_normalized_typescript_adapter_and_barrel_callers(self):
        for specifier in ("../platform/os", "../platform/os.ts", "../platform/./os.ts",
                          "../platform/../platform/os.ts", "../platform/os.js", "../barrel"):
            with self.subTest(specifier=specifier), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                self.desktop_fixture(root)
                self.fixture(root, "apps/desktop/src/barrel/index.ts", 'export { openPath } from "../bridge.js";')
                self.fixture(root, "apps/desktop/src/bridge.ts", 'export * from "./platform/os.ts";')
                baseline = check.inventory(root)
                self.assert_new_caller_rejected(root, baseline, "apps/desktop/src/ui/preview.ts",
                    f'import {{ openPath }} from "{specifier}"; export const openHostFile = () => openPath("/host/file");')

    def test_new_reexports_are_also_inventoried(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            self.desktop_fixture(root)
            baseline = check.inventory(root)
            self.assert_new_caller_rejected(root, baseline, "apps/desktop/src/bridge.ts",
                                           'export * from "./platform/os.ts";')
            self.assert_new_caller_rejected(root, baseline, "apps/desktop/src-tauri/src/bridge.rs",
                                           'pub use crate::commands::read_image;')

    def test_template_interpolation_io_fails_inventory_for_new_and_previously_clean_files(self):
        for expression in [
            '`${await (await fetch("https://host/data")).text()}`',
            '`outer ${`inner ${fetch(url)}`} tail`',
            '`${({ value: "}", load: () => { /* } */ return fetch(url); } }).load()}`',
            '`${await import("@tauri-apps/plugin-fs")}`',
            '`${await import("./platform/os.ts")}`',
            '`${ /[}`]/.test(value) ? fetch(url) : "" }`',
            '`${value / 2 + (await fetch(url)).status}`',
        ]:
            for previously_clean in (False, True):
                with self.subTest(expression=expression, previously_clean=previously_clean), tempfile.TemporaryDirectory() as folder:
                    root = Path(folder)
                    self.desktop_fixture(root)
                    name = "apps/desktop/src/preview.ts"
                    if previously_clean:
                        self.fixture(root, name, 'export const load = () => "safe";')
                    baseline = check.inventory(root)
                    self.assertNotIn(name, baseline)
                    self.assert_new_caller_rejected(root, baseline, name,
                                                   'export const load = async () => ' + expression + ';')

    def test_postfix_non_null_division_cannot_hide_later_io_from_inventory(self):
        review_fixture = """export const load = (n: number | undefined, url: string) => {
  const label = `${n! / 2}`;
  return fetch(url).then(() => label);
};"""
        for source in [
            review_fixture,
            review_fixture.replace("n! / 2", "n! /* assertion */ / 2 / 3"),
            review_fixture.replace("n! / 2", "(n!)! / 2"),
            review_fixture.replace("`${n! / 2}`", "`outer ${`inner ${n! / 2}`}`"),
            review_fixture.replace("return fetch(url).then(() => label);",
                                   'fetch(url); const pattern = /safe/; return label;'),
        ]:
            for previously_clean in (False, True):
                with self.subTest(source=source, previously_clean=previously_clean), tempfile.TemporaryDirectory() as folder:
                    root = Path(folder)
                    self.desktop_fixture(root)
                    name = "apps/desktop/src/verification-preview.ts"
                    if previously_clean:
                        self.fixture(root, name, 'export const load = () => "safe";')
                    baseline = check.inventory(root)
                    self.assertNotIn(name, baseline)
                    self.assert_new_caller_rejected(root, baseline, name, source)
                    self.assertIn("fetch", check.inventory(root)[name]["reasons"])

    def test_prefix_negation_regex_and_nested_interpolation_remain_supported(self):
        for expression in [
            '`${!/}/.test(value) ? fetch(url) : ""}`',
            '`${!!/[}`]/.test(value) ? fetch(url) : ""}`',
            r'`${!/a\/[}`]/.test(value) ? fetch(url) : ""}`',
            '`outer ${`inner ${!/}/.test(value) ? fetch(url) : ""}`}`',
        ]:
            with self.subTest(expression=expression), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                self.desktop_fixture(root)
                baseline = check.inventory(root)
                self.assert_new_caller_rejected(root, baseline, "apps/desktop/src/preview.ts",
                                               'export const load = () => ' + expression + ';')
        # Regex contents stay inert; prefix ! and !! must not be treated as
        # postfix assertions, even with regex braces/backticks and I/O words.
        for source in ['`${!/fetch[}`]/.test(value)}`', '`${!!/fetch[}`]/.test(value)}`']:
            with self.subTest(source=source): self.assertEqual(check.risks(source, ".ts"), [])

    def test_unterminated_regex_guess_does_not_consume_later_code(self):
        # Even incomplete input must not let a guessed regex swallow later I/O.
        for guessed_regex in ["/unfinished", "/[unfinished", "/unfinished\\"]:
            with self.subTest(guessed_regex=guessed_regex):
                source = '`${!' + guessed_regex + '\n}`; fetch(url);'
                self.assertIn("fetch", check.risks(source, ".ts"))

    def test_template_literal_text_and_escaped_interpolations_remain_inert(self):
        for source in [
            '`fetch(url) $\\{fetch(url)} \\${fetch(url)}`',
            '`text ${"fetch(url)"} ${/* fetch(url) */ 1}`',
            '`text ${`nested ${"fetch(url)"}`} tail`',
        ]:
            with self.subTest(source=source): self.assertEqual(check.risks(source, ".ts"), [])

    def test_contract_dependency_and_io_rules_are_enforced(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            crate = root / 'crates/skiff-host-api'
            (crate / 'src').mkdir(parents=True)
            manifest = '[dependencies]\nserde.workspace = true\nbase64.workspace = true\n'
            (crate / 'Cargo.toml').write_text(manifest)
            (crate / 'src/lib.rs').write_text('use std::fmt;')
            self.assertEqual(check.api_errors(root), [])
            (crate / 'Cargo.toml').write_text(manifest + 'tokio.workspace = true\n')
            self.assertTrue(check.api_errors(root))
            (crate / 'Cargo.toml').write_text(manifest)
            (crate / 'src/lib.rs').write_text('std::fs::read(path);')
            self.assertTrue(check.api_errors(root))

    def test_refresh_updates_only_the_named_existing_entry(self):
        old = lambda h: {'sha256': h, 'reasons': ['dependency surface'], 'ownership': 'reviewed'}
        baseline = {'apps/desktop/package.json': old('a'), 'src/x.ts': old('b')}
        actual = {'apps/desktop/package.json': {'sha256': 'A', 'reasons': ['dependency surface']},
                  'src/x.ts': {'sha256': 'B', 'reasons': ['invoke']}, 'src/new.ts': {'sha256': 'N', 'reasons': ['fetch']}}
        updated = check.refresh(baseline, actual, ['apps/desktop/package.json'])
        self.assertEqual(updated['apps/desktop/package.json'], {**old('A')})
        self.assertEqual(updated['src/x.ts'], old('b'))
        self.assertEqual(len(check.compare(actual, updated)), 2)
        with self.assertRaises(SystemExit): check.refresh(baseline, actual, ['src/new.ts'])


if __name__ == '__main__': unittest.main()
