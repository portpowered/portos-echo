export interface Slot {
    x: number;
    y: number;
    color?: string;
    font_scale?: number;
}
export interface Layout {
    safe_area: { x: number; y: number; width: number; height: number };
    slots: Partial<
        Record<
            "clock" | "date" | "status" | "link_code" | "approval" | "error",
            Slot
        >
    >;
    stage: {
        anchor: { x: number; y: number };
        scale: number;
        background: string;
    };
    css?: string;
}

const nativeCardStates = ["link_code", "approval", "error"] as const;
type NativeCardState = (typeof nativeCardStates)[number];

function fitSideSlot(node: HTMLElement, slot: Slot) {
    node.style.right = "12px";
    node.style.maxHeight = `calc(${100 - slot.y}% - 12px)`;
}

function fitSideCard(card: HTMLElement, controls: HTMLElement | null) {
    const messages = card.querySelectorAll<HTMLElement>(":scope > p");
    for (const message of messages) {
        message.style.fontSize = "inherit";
        message.style.margin = "0 0 8px";
    }
    if (!controls?.childElementCount) return;
    card.style.display = "flex";
    card.style.flexDirection = "column";
    card.style.overflow = "hidden";
    for (const message of messages) {
        message.style.minHeight = "0";
        message.style.overflowY = "auto";
    }
    controls.style.gap = "12px";
    controls.style.flexWrap = "nowrap";
    for (const button of controls.querySelectorAll("button")) {
        button.style.flex = "1 1 0";
        button.style.minWidth = "0";
        button.style.maxWidth = "none";
        button.style.marginBlock = "8px";
        button.style.paddingInline = "8px";
        button.style.fontSize = "20px";
    }
}

// Content remains the canonical native controls. Position its overlays inside
// the safe area; scroll oversized text instead of clipping approval actions.
export function applyLayout(host: HTMLElement, layout: Layout | undefined) {
    const feedback = host.nextElementSibling;
    const clock = document.createElement("p");
    clock.className = "pack-clock";
    clock.dataset.packPersistent = "";
    const date = document.createElement("p");
    date.className = "pack-date";
    date.dataset.packPersistent = "";
    host.append(clock, date);
    const area = layout?.safe_area ?? { x: 0, y: 0, width: 100, height: 100 };
    host.style.setProperty("--pack-x", `${area.x}vw`);
    host.style.setProperty("--pack-y", `${area.y}dvh`);
    host.style.setProperty("--pack-width", `${area.width}vw`);
    host.style.setProperty("--pack-height", `${area.height}dvh`);
    function slot(node: HTMLElement, name: keyof Layout["slots"]) {
        node.classList.add(`pack-${name.replace(/_/g, "-")}`);
        const value = layout?.slots[name];
        if (!value) return;
        node.style.position = "absolute";
        node.style.left = `${value.x}%`;
        node.style.top = `${value.y}%`;
        if (value.color) node.style.color = value.color;
        if (value.font_scale)
            node.style.fontSize = `${28 * value.font_scale}px`;
        return value;
    }
    // A slot right of the stage is beside it. Over a full-page stage, a card
    // slot in the right half is fitted to the right edge as well.
    function besideStage(value: Slot | undefined, card = false): value is Slot {
        if (!value || value.x >= 100 || value.y >= 100 || !layout) return false;
        if (!host.querySelector(".pack-stage")) return false;
        const right = layout.stage.anchor.x + 25 * layout.stage.scale;
        return value.x >= (card ? Math.min(50, right) : right);
    }
    function placeFeedback(status?: Slot) {
        // The page keeps its live region immediately after the presentation.
        if (
            !(feedback instanceof HTMLElement) ||
            !feedback.matches('[role="status"]')
        )
            return;
        const { style } = feedback;
        if (!status) {
            style.position = style.inset = style.width = style.margin = "";
            return;
        }
        const left = area.x + (status.x / 100) * area.width;
        const top = area.y + (status.y / 100) * area.height;
        const right = 100 - area.x - area.width;
        style.position = "fixed";
        style.inset = `${top}dvh calc(${right}vw + 12px) auto ${left}vw`;
        style.width = "auto";
        style.margin = "0";
    }
    slot(clock, "clock");
    slot(date, "date");
    clock.style.width = date.style.width = "max-content";
    // Time and date anchor at their slot x by their own text-align: centred
    // text centres its box on x, so both lines share one centre line.
    function anchor(node: HTMLElement, width: number) {
        if (node !== clock && node !== date) return 0;
        const align = getComputedStyle(node).textAlign;
        if (align === "center") return width / 2;
        return align === "right" || align === "end" ? width : 0;
    }
    function clamp() {
        for (const node of host.querySelectorAll<HTMLElement>(
            "[data-pack-slot]",
        )) {
            node.style.transform = "";
            const box = node.getBoundingClientRect();
            const bounds = host.getBoundingClientRect();
            const left = box.left - anchor(node, box.width);
            const x =
                left -
                box.left +
                Math.max(
                    Math.min(0, bounds.right - (left + box.width)),
                    bounds.left - left,
                );
            const y = Math.min(0, bounds.bottom - box.bottom);
            node.style.transform = `translate(${x}px, ${y}px)`;
        }
    }
    clock.dataset.packSlot = date.dataset.packSlot = "";
    const observer = new ResizeObserver(clamp);
    observer.observe(host);
    function tick() {
        const now = new Date();
        clock.textContent = now.toLocaleTimeString([], {
            hour: "2-digit",
            minute: "2-digit",
        });
        date.textContent = now.toLocaleDateString();
        clamp();
    }
    tick();
    const timer = window.setInterval(tick, 1000);
    return {
        render(state: string) {
            const idle = ["ready", "connected"].includes(state);
            // Time and date stay put through every state.
            clock.hidden = date.hidden = false;
            const feedbackSlot = layout?.slots.status;
            placeFeedback(
                idle && besideStage(feedbackSlot) ? feedbackSlot : undefined,
            );
            const heading = host.querySelector<HTMLElement>("h1");
            const details = host.querySelector<HTMLElement>(".details");
            if (heading) {
                const status = slot(heading, "status");
                heading.dataset.packSlot = "";
                if (besideStage(status, true)) {
                    fitSideSlot(heading, status);
                    heading.style.lineHeight = "1.4";
                }
            }
            if (
                details &&
                nativeCardStates.includes(state as NativeCardState)
            ) {
                const anchor = slot(details, state as NativeCardState);
                details.dataset.packSlot = "";
                // Native buttons stay with their prompt inside one scrollable card.
                const controls = host.querySelector<HTMLElement>(".controls");
                if (controls) details.append(controls);
                if (besideStage(anchor, true)) {
                    fitSideSlot(details, anchor);
                    fitSideCard(details, controls);
                }
            }
            observer.disconnect();
            observer.observe(host);
            for (const node of host.querySelectorAll<HTMLElement>(
                "[data-pack-slot]",
            ))
                observer.observe(node);
            clamp();
        },
        stage() {
            const node = host.querySelector<HTMLElement>(".pack-stage");
            if (!node || !layout) return;
            node.style.left = `${layout.stage.anchor.x}%`;
            node.style.top = `${layout.stage.anchor.y}%`;
            node.style.width = `${50 * layout.stage.scale}%`;
            node.style.height = `${50 * layout.stage.scale}%`;
            node.style.backgroundColor = layout.stage.background;
        },
        refresh: clamp,
        dispose() {
            observer.disconnect();
            clearInterval(timer);
            clock.remove();
            date.remove();
            placeFeedback();
            for (const key of ["x", "y", "width", "height"])
                host.style.removeProperty(`--pack-${key}`);
        },
    };
}
