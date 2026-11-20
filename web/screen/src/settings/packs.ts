import { decodeSaved } from "./api";
import { element } from "./forms";
import {
    decodeJob,
    decodePacks,
    packRequest,
    previewUrl,
    type PackSnapshot,
} from "./packs-api";

// The default eyes ship as "default"; Echos that installed them before the
// rename still have "template". The bridge resolves either name to the newest
// default pack, preferring "default".
const DEFAULT_NAMES = ["default", "template"];
const isDefault = (name: string) => DEFAULT_NAMES.includes(name);

type Perform = (
    work: (signal: AbortSignal) => Promise<string>,
    message?: string,
) => Promise<void>;

// Uses the settings panel's one pending request, cancellation and focus owner.
// Explicit refresh avoids an unbounded polling task when settings are hidden.
export function packControls(actions: string, perform: Perform): HTMLElement {
    const section = element("section");
    section.className = "settings-packs";
    section.setAttribute("aria-label", "Presentation");
    const catalog = element("div");
    const refresh = element("button", "Load screen packs");
    refresh.type = "button";
    section.append(element("h2", "Presentation"), refresh, catalog);
    const load = async (signal: AbortSignal) => {
        const data = decodePacks(
            await packRequest(actions, "", signal),
            actions,
        );
        if (!signal.aborted) {
            render(data);
        }
        return data;
    };
    refresh.addEventListener("click", () => {
        void perform(async (signal) => {
            await load(signal);
            return "Screen packs loaded";
        }, "Loading screen packs…");
    });
    function render(data: PackSnapshot) {
        catalog.replaceChildren();
        refresh.textContent = "Refresh packs";
        const select = element("select");
        const label = element("label", "Screen pack");
        select.id = "screen-pack-selection";
        label.htmlFor = select.id;
        const automatic = element("option", "Default");
        automatic.value = "default";
        automatic.disabled = !data.installed.some((pack) =>
            isDefault(pack.name),
        );
        select.append(automatic);
        const installed = [
            ...data.installed.filter((pack) => isDefault(pack.name)),
            ...data.installed.filter((pack) => !isDefault(pack.name)),
        ];
        for (const pack of installed) {
            // An unversioned directory named like the default resolves as
            // the default itself, so it gets no option of its own.
            if (!isDefault(pack.id)) {
                const option = element(
                    "option",
                    `${pack.name} ${pack.version}`,
                );
                option.value = pack.id;
                select.append(option);
            }
            const item = element("div");
            item.append(element("p", `${pack.name} ${pack.version}`));
            if (pack.rest_url) {
                const image = element("img");
                image.alt = `${pack.name} rest pose`;
                image.referrerPolicy = "no-referrer";
                image.loading = "lazy";
                image.src = previewUrl(actions, pack.id, pack.rest_url);
                image.addEventListener(
                    "error",
                    () => {
                        image.replaceWith(element("p", "Preview unavailable"));
                    },
                    { once: true },
                );
                item.append(image);
            } else
                item.append(element("p", "Layout only — no character preview"));
            catalog.append(item);
        }
        const disable = element("option", "Disable screen packs");
        disable.value = "";
        select.append(disable);
        select.value = data.active ?? "";
        const save = element("button", "Use screen pack");
        const form = element("form");
        form.append(label, select, save);
        form.addEventListener("submit", (event) => {
            event.preventDefault();
            const selection = select.value;
            if (select.selectedOptions[0]?.disabled) return;
            void perform(async (signal) => {
                decodeSaved(
                    await packRequest(actions, "/selection", signal, {
                        version: 1,
                        screen_pack: selection,
                    }),
                );
                await load(signal);
                return "Screen pack saved. Appearance updates without restarting the bridge.";
            }, "Saving screen pack…");
        });
        if (!data.installed.length)
            catalog.append(element("p", "No screen packs installed"));
        if (data.warning)
            catalog.append(
                element(
                    "p",
                    "The selected pack is unavailable. Using the built-in appearance.",
                ),
            );
        catalog.append(form, installForm(actions, perform));
        for (const job of data.jobs)
            catalog.append(
                element(
                    "p",
                    `Download ${job.id}: ${job.status}${job.reason ? ` (${job.reason.replace(/_/g, " ")})` : ""}`,
                ),
            );
    }
    return section;
}

function installForm(actions: string, perform: Perform) {
    const form = element("form");
    const url = element("input");
    Object.assign(url, {
        type: "url",
        required: true,
        pattern: "https://.*",
        maxLength: 4096,
    });
    const sha = element("input");
    Object.assign(sha, {
        type: "text",
        required: true,
        pattern: "[a-fA-F0-9]{64}",
        minLength: 64,
        maxLength: 64,
        spellcheck: false,
    });
    const urlLabel = element("label", "Pack HTTPS URL");
    const shaLabel = element("label", "Pack SHA-256");
    urlLabel.append(url);
    shaLabel.append(sha);
    form.append(
        element("h3", "Add from URL"),
        urlLabel,
        shaLabel,
        element("button", "Download pack"),
    );
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        const body = { version: 1, url: url.value, sha256: sha.value };
        void perform(async (signal) => {
            const job = decodeJob(
                await packRequest(actions, "/install", signal, body),
            );
            if (!signal.aborted) {
                url.value = "";
                sha.value = "";
            }
            return `Download ${job} queued. Refresh packs to check progress.`;
        }, "Queueing download…");
    });
    return form;
}
