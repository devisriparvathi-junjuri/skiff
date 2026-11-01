import { open as openFolder } from "@tauri-apps/plugin-dialog";
import { showError } from "../ui/alerts";
import { actionDialog } from "../ui/dialogParts";
import { S } from "../app/state";
import { openFolderPicker } from "./folderPicker";
import { selectProject } from "./view";
import { hostInvoke } from "../app/hostCaps";
import { hostName, isRemote } from "../app/hosts";
import type { FolderInfo, Project } from "../platform/types";
import { t } from "../i18n";

/**
 * Pick a folder, check it with the Host, and add it with the defaults: the
 * repo's or folder's name, letters or its own icon, a random free color and
 * theme. The project's right-click menu changes them afterwards. A folder in
 * no git repository becomes a plain project. The Host checks and adds it
 * (`project_config@1`): on This Mac a folder from the system dialog, on a
 * remote Host one from the folder picker, never a path of this Mac.
 */
/** `fleet`: the fleet the new project joins, from the + on a fleet head. */
export function createAddProject(onAdded: (p: Project, fleet: string | null) => void, onClose: () => void) {
  let busy = false;
  let fleet: string | null = null;
  async function add(folder: FolderInfo) {
    if (!folder.root || folder.error) throw new Error(folder.error ?? t("project.pickFolder"));
    const p = await hostInvoke<Project>("project_config", "add_project", {
      path: folder.root,
      name: folder.name,
      short: folder.short || null,
      color: folder.color,
    });
    await onAdded(p, fleet);
  }
  /** A remote Host: its folder, picked from the Host's own disk. The
   *  picker checks each folder with the Host before the button acts. */
  function addRemote() {
    const host = hostName();
    const inspect = (path: string) => hostInvoke<FolderInfo>("project_config", "inspect_folder", { path });
    openFolderPicker({
      action: t("project.add"),
      start: "repos",
      check: async (path, name) => {
        const folder = await inspect(path);
        if (!folder.root) {
          return {
            label: t("project.initRepo"),
            note: t("project.notRepo", { name }),
            run: () => initRemote(path, name, host),
          };
        }
        const known = S.projects.find((p) => p.path === folder.root);
        if (known) {
          return {
            label: t("project.showIt"),
            note: t("project.already", { name }),
            run: async () => {
              selectProject(known.name);
              return true;
            },
          };
        }
        return null;
      },
      pick: async (path) => add(await inspect(path)),
      onClose: (picked) => {
        if (!picked) onClose();
      },
    });
  }
  /** Asks first, then runs `git init` there through the Host and adds it. */
  function initRemote(path: string, name: string, host: string): Promise<boolean> {
    return new Promise((resolve) => {
      let done = false;
      actionDialog({
        title: t("project.initTitle", { name }),
        body: t("project.initBody", { path, host }),
        action: t("project.initAction"),
        tone: "go",
        submit: async () => {
          await add(await hostInvoke<FolderInfo>("project_config", "init_host_repository", { path }));
          done = true;
        },
        onClose: () => resolve(done),
      });
    });
  }
  return {
    async open(into: string | null = null) {
      if (busy) return;
      fleet = into;
      if (isRemote()) return addRemote();
      busy = true;
      try {
        const dir = await openFolder({ directory: true, title: t("project.pickFolderTitle") });
        if (typeof dir !== "string") return onClose();
        const folder = await hostInvoke<FolderInfo>("project_config", "inspect_folder", { path: dir });
        await add(folder);
      } catch (e) {
        showError(e);
        onClose();
      } finally {
        busy = false;
      }
    },
    get isOpen() {
      return busy;
    },
  };
}
