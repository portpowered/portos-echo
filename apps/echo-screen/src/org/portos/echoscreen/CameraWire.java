package org.portos.echoscreen;

import java.io.IOException;
import java.io.Reader;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.MessageDigest;
import java.util.HashMap;
import java.util.HashSet;
import java.util.Map;
import java.util.Set;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import javax.crypto.Mac;
import javax.crypto.spec.SecretKeySpec;

final class CameraWire {
    static final String NONCE = "X-Echo-Camera-Nonce";
    static final String PROOF = "X-Echo-Camera-Proof";
    static final int MAX_JPEG = 4 * 1024 * 1024;
    private static final Pattern ID = Pattern.compile("[0-9a-f]{32}");
    private static final Pattern PROOF_TEXT = Pattern.compile("[0-9a-f]{64}");
    private static final Pattern FIELD = Pattern.compile(
            "\\s*\"([A-Za-z]+)\"\\s*:\\s*(?:\"((?:[^\"\\\\\\x00-\\x1f]|\\\\[\"\\\\/bfnrt]|\\\\u[0-9a-fA-F]{4})*)\"|([0-9]+))\\s*([,}])");
    private static final char[] HEX = "0123456789abcdef".toCharArray();

    final String nonce;
    private final SecretKeySpec key;

    CameraWire(Secret token, String nonce) throws GeneralSecurityException {
        this.nonce = nonce;
        key = new SecretKeySpec(digest(token.expose().getBytes(StandardCharsets.UTF_8)), "HmacSHA256");
    }

    private byte[] sign(String direction, String... fields) throws GeneralSecurityException {
        Mac mac = Mac.getInstance("HmacSHA256");
        mac.init(key);
        mac.update(("echo-screen-camera/v1\n" + direction + "\n" + nonce).getBytes(StandardCharsets.UTF_8));
        for (String field : fields) {
            mac.update((byte) '\n');
            mac.update(field.getBytes(StandardCharsets.UTF_8));
        }
        return mac.doFinal();
    }

    String attach() throws GeneralSecurityException {
        return hex(sign("app", "attach"));
    }

    String result(String id, String contentType, String width, String height, String sampledAt, byte[] body)
            throws GeneralSecurityException {
        return hex(sign("app", "result", id, contentType, width, height, sampledAt, hex(digest(body))));
    }

    final class Stream {
        private final Set<String> seen = new HashSet<>();
        private boolean ready;

        Frame next(Reader input) throws IOException, GeneralSecurityException {
            Frame checked = frame(input);
            if (!ready) {
                if (!"ready".equals(checked.event)) throw new IOException();
                ready = true;
            } else if ("ready".equals(checked.event)
                    || ("capture".equals(checked.event) && (seen.size() == 256 || !seen.add(checked.id)))) {
                throw new IOException();
            }
            return checked;
        }
    }

    Frame frame(Reader input) throws IOException, GeneralSecurityException {
        String event = null;
        String data = null;
        String proof = null;
        for (;;) {
            String line = line(input);
            if (line.isEmpty()) {
                if (event == null && data == null && proof == null) continue;
                break;
            }
            if (line.startsWith(":")) continue;
            if (line.startsWith("event: ") && event == null) event = line.substring(7);
            else if (line.startsWith("data: ") && data == null) data = line.substring(6);
            else if (line.startsWith("id: ") && proof == null) proof = line.substring(4);
            else throw new IOException();
        }
        if (event == null || data == null || proof == null || !PROOF_TEXT.matcher(proof).matches()
                || !MessageDigest.isEqual(hex(sign("bridge", "event", event, data))
                        .getBytes(StandardCharsets.US_ASCII), proof.getBytes(StandardCharsets.US_ASCII))) {
            throw new IOException();
        }
        return decode(event, data);
    }

    private static String line(Reader input) throws IOException {
        StringBuilder line = new StringBuilder();
        while (line.length() < 32768) {
            int next = input.read();
            if (next == '\n') return line.toString();
            if (next < 0 || next == '\r') break;
            line.append((char) next);
        }
        throw new IOException();
    }

    private static Frame decode(String event, String data) throws IOException {
        Map<String, String> fields = object(data);
        if ("ready".equals(event) && fields.isEmpty()) return new Frame(event, null, null, 0);
        String id = fields.get("captureId");
        if (id == null || !ID.matcher(id).matches()) throw new IOException();
        if ("cancel".equals(event) && fields.size() == 1) return new Frame(event, id, null, 0);
        if (!"capture".equals(event) || fields.size() != 3) throw new IOException();
        String camera = fields.get("cameraId");
        String remaining = fields.get("remainingMs");
        if (camera == null || camera.trim().isEmpty() || camera.codePoints().anyMatch(Character::isISOControl)
                || remaining == null || !remaining.matches("[1-9][0-9]{0,3}")) throw new IOException();
        int milliseconds = Integer.parseInt(remaining);
        if (milliseconds > 3000) throw new IOException();
        return new Frame(event, id, camera, milliseconds);
    }

    private static Map<String, String> object(String data) throws IOException {
        Map<String, String> fields = new HashMap<>();
        if ("{}".equals(data)) return fields;
        if (!data.startsWith("{")) throw new IOException();
        Matcher field = FIELD.matcher(data);
        int offset = 1;
        while (field.region(offset, data.length()).lookingAt()) {
            String key = field.group(1);
            String value = field.group(2);
            if ("remainingMs".equals(key) ? value != null : value == null) throw new IOException();
            if (fields.put(key, value == null ? field.group(3) : unescape(value)) != null) throw new IOException();
            offset = field.end();
            if ("}".equals(field.group(4)) && offset == data.length()) return fields;
            if (!",".equals(field.group(4))) break;
        }
        throw new IOException();
    }

    private static String unescape(String value) throws IOException {
        StringBuilder out = new StringBuilder();
        for (int index = 0; index < value.length(); index++) {
            char ch = value.charAt(index);
            if (ch != '\\') {
                out.append(ch);
                continue;
            }
            char next = value.charAt(++index);
            if (next == 'u') {
                out.append((char) Integer.parseInt(value.substring(index + 1, index + 5), 16));
                index += 4;
            } else if (next == '\\' || next == '\"' || next == '/') {
                out.append(next);
            } else {
                // Camera IDs and capture IDs never contain escaped controls.
                throw new IOException();
            }
        }
        return out.toString();
    }

    private static byte[] digest(byte[] bytes) throws GeneralSecurityException {
        return MessageDigest.getInstance("SHA-256").digest(bytes);
    }

    static String hex(byte[] bytes) {
        char[] encoded = new char[bytes.length * 2];
        for (int index = 0; index < bytes.length; index++) {
            encoded[index * 2] = HEX[(bytes[index] >>> 4) & 15];
            encoded[index * 2 + 1] = HEX[bytes[index] & 15];
        }
        return new String(encoded);
    }

    static final class Frame {
        final String event;
        final String id;
        final String camera;
        final int remaining;

        Frame(String event, String id, String camera, int remaining) {
            this.event = event;
            this.id = id;
            this.camera = camera;
            this.remaining = remaining;
        }
    }
}
