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

type Pack = PackSnapshot["installed"][number];

function packCard(actions: string, pack: Pack) {
    const title = `${pack.name} ${pack.version}`;
    const card = element("button");
    card.type = "button";
    card.className = "pack-card";
    card.setAttribute("aria-label", title);
    const preview = element("span");
    preview.className = "pack-preview";
    if (pack.rest_url) {
        const image = element("img");
        image.alt = `${pack.name} rest pose`;
        image.referrerPolicy = "no-referrer";
        image.loading = "lazy";
        image.addEventListener(
            "error",
            () => {
                image.replaceWith(element("span", "Preview unavailable"));
            },
            { once: true },
        );
        image.src = previewUrl(actions, pack.id, pack.rest_url);
        preview.append(image);
    } else {
        preview.append(element("span", "Layout only — no character preview"));
    }
    const marker = element("span", "✓ Selected");
    marker.className = "pack-marker";
    marker.setAttribute("aria-hidden", "true");
    card.append(preview, element("span", title), marker);
    return card;
}

function mountCarousel(
    catalog: HTMLElement,
    actions: string,
    packs: Pack[],
    active: string | null,
    select: HTMLSelectElement,
) {
    const rail = element("div");
    rail.className = "pack-rail";
    rail.setAttribute("role", "group");
    rail.setAttribute("aria-label", "Installed screen packs");
    const cards = packs.map((pack) => ({
        pack,
        button: packCard(actions, pack),
    }));
    const navigation = element("div");
    navigation.className = "pack-navigation";
    const previous = element("button", "Previous");
    const next = element("button", "Next");
    previous.type = next.type = "button";
    let current = 0;

    function mark(selected: number) {
        if (selected !== -1) current = selected;
        cards.forEach(({ button }, index) => {
            button.tabIndex = index === current ? 0 : -1;
            button.setAttribute("aria-pressed", String(index === selected));
        });
        previous.setAttribute("aria-disabled", String(current === 0));
        next.setAttribute(
            "aria-disabled",
            String(current === cards.length - 1),
        );
        const selectedButton = cards[selected]?.button;
        if (!selectedButton) return;
        const card = selectedButton.getBoundingClientRect();
        const viewport = rail.getBoundingClientRect();
        // Scroll the rail only. scrollIntoView can also move the Settings panel.
        rail.scrollBy({
            left:
                card.left + card.width / 2 - viewport.left - viewport.width / 2,
        });
    }

    function pick(index: number, focus = false) {
        const card = cards[index];
        if (!card) return;
        const { id } = card.pack;
        select.value = isDefault(id) ? "default" : id;
        mark(index);
        if (focus) card.button.focus({ preventScroll: true });
    }

    cards.forEach(({ button }, index) => {
        button.addEventListener("click", () => pick(index));
        button.addEventListener("keydown", (event) => {
            if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
            event.preventDefault();
            pick(index + (event.key === "ArrowLeft" ? -1 : 1), true);
        });
    });
    select.addEventListener("change", () => {
        // Default is an alias. Only the catalog confirms its effective id.
        mark(
            select.value === "default"
                ? -1
                : packs.findIndex((pack) => pack.id === select.value),
        );
    });
    previous.addEventListener("click", () => pick(current - 1));
    next.addEventListener("click", () => pick(current + 1));
    rail.append(...cards.map(({ button }) => button));
    catalog.prepend(rail);
    if (cards.length > 1) {
        navigation.append(previous, next);
        rail.after(navigation);
    }
    mark(packs.findIndex((pack) => pack.id === active));
}

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
        }
        const disable = element("option", "Disable screen packs");
        disable.value = "";
        select.append(disable);
        select.value =
            data.active && isDefault(data.active)
                ? "default"
                : (data.active ?? "");
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
        if (!installed.length)
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
        if (installed.length)
            mountCarousel(catalog, actions, installed, data.active, select);
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
