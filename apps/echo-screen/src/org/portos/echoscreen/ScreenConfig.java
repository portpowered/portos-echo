package org.portos.echoscreen;

import java.net.URI;
import java.net.URISyntaxException;

final class ScreenConfig {
    static final int TOP = 16;
    static final int BOTTOM = 10;
    static final String WAITING = "Waiting for bridge";
    static final String DIAGNOSTIC = "url=[redacted]";
    final Secret url;
    final String message;

    interface Store {
        Secret read();
        void save(Secret url);
    }

    private ScreenConfig(Secret url, String message) {
        this.url = url;
        this.message = message;
    }

    enum Chrome {
        LEGACY(TOP, BOTTOM), PAGE(0, 0);

        final int top;
        final int bottom;

        Chrome(int top, int bottom) {
            this.top = top;
            this.bottom = bottom;
        }
    }

    static Chrome chrome(boolean legacyHome, boolean fullPanel, boolean trustedDocument) {
        return !legacyHome && fullPanel && trustedDocument ? Chrome.PAGE : Chrome.LEGACY;
    }

    static ScreenConfig select(boolean supplied, Secret extra, Store store) {
        Secret selected = supplied ? extra : store.read();
        if (selected == null && !supplied) {
            return new ScreenConfig(null, "Screen URL is not configured");
        }
        if (!valid(selected)) {
            return new ScreenConfig(null, "Screen URL is invalid");
        }
        if (supplied) {
            store.save(selected);
        }
        return new ScreenConfig(selected, WAITING);
    }

    private static boolean valid(Secret value) {
        if (value == null) return false;
        try {
            URI uri = new URI(value.expose());
            return ("http".equalsIgnoreCase(uri.getScheme())
                    || "https".equalsIgnoreCase(uri.getScheme()))
                    && uri.getHost() != null && uri.getRawUserInfo() == null
                    && uri.getPort() <= 65535;
        } catch (URISyntaxException ignored) {
            return false;
        }
    }
}
