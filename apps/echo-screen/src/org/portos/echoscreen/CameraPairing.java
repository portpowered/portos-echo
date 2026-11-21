package org.portos.echoscreen;

import java.net.URI;
import java.net.URISyntaxException;
import java.net.URLDecoder;
import java.io.UnsupportedEncodingException;
import java.util.HashMap;
import java.util.Map;

/** Only URLs supplied to the protected activity may bootstrap native capture. */
final class CameraPairing {
    final String origin;
    final Secret token;

    private CameraPairing(String origin, String token) {
        this.origin = origin;
        this.token = new Secret(token);
    }

    static CameraPairing parse(Secret page) {
        if (page == null) return null;
        try {
            URI uri = new URI(page.expose());
            if (!"http".equals(uri.getScheme()) || !"127.0.0.1".equals(uri.getHost())
                    || uri.getRawUserInfo() != null || uri.getRawFragment() != null
                    || uri.getPort() < 1 || uri.getPort() > 65535) return null;
            Map<String, String> endpoints = query(uri.getRawQuery());
            if (endpoints.size() != 2) return null;
            String events = token(endpoints.get("events"), "/events");
            String actions = token(endpoints.get("actions"), "/actions");
            if (events == null || !events.equals(actions)) return null;
            return new CameraPairing("http://127.0.0.1:" + uri.getPort(), events);
        } catch (URISyntaxException | IllegalArgumentException | UnsupportedEncodingException ignored) {
            return null;
        }
    }

    private static String token(String endpoint, String path)
            throws URISyntaxException, UnsupportedEncodingException {
        if (endpoint == null) return null;
        URI uri = new URI(endpoint);
        if (!path.equals(uri.getRawPath()) || uri.getRawAuthority() != null
                || uri.getScheme() != null || uri.getRawFragment() != null) return null;
        Map<String, String> fields = query(uri.getRawQuery());
        String token = fields.get("token");
        return fields.size() == 1 && token != null && !token.trim().isEmpty()
                && token.codePoints().noneMatch(Character::isISOControl) ? token : null;
    }

    private static Map<String, String> query(String text) throws UnsupportedEncodingException {
        if (text == null) throw new IllegalArgumentException();
        Map<String, String> values = new HashMap<>();
        for (String entry : text.split("&", -1)) {
            int equals = entry.indexOf('=');
            if (equals < 1) throw new IllegalArgumentException();
            String key = URLDecoder.decode(entry.substring(0, equals), "UTF-8");
            String value = URLDecoder.decode(entry.substring(equals + 1), "UTF-8");
            if (values.put(key, value) != null) throw new IllegalArgumentException();
        }
        return values;
    }
}
