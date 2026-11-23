// Android Wi-Fi reflection calls adapted from echo-dot-assist (MIT),
// portpowered/echo-dot-assist@7e0c758 tools/mkwifictl.py.
// The copyright and licence ship with echo-bridge notices.
import android.net.ConnectivityManager;
import android.net.LinkAddress;
import android.net.LinkProperties;
import android.net.RouteInfo;
import android.net.wifi.ScanResult;
import android.net.wifi.SupplicantState;
import android.net.wifi.WifiConfiguration;
import android.net.wifi.WifiInfo;
import android.os.SystemClock;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Objects;
import java.util.function.Predicate;
import org.json.JSONArray;
import org.json.JSONObject;

public final class EchoWifi {
    private static final String PACKAGE = "android";

    public static void main(String[] args) {
        Thread watchdog = new Thread(() -> {
            SystemClock.sleep(args.length == 1 ? 60000 : 8000);
            Runtime.getRuntime().halt(1);
        });
        watchdog.setDaemon(true);
        watchdog.start();
        try (BufferedReader input = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8))) {
            Object wifi = service("wifi", "android.net.wifi.IWifiManager");
            JoinAttempt attempt = new JoinAttempt(wifi);
            for (String line; (line = input.readLine()) != null;) {
                try {
                    System.out.println(command(wifi, attempt, new JSONObject(line)));
                } catch (Exception e) {
                    // Send a fixed response and leave the pipe available for rollback.
                    System.out.println(JSONObject.NULL);
                }
            }
        } catch (Exception e) {
            // Framework diagnostics can include network names. The caller
            // maps absent or invalid output to the fixed unavailable result.
            System.exit(1);
        }
        System.exit(0);
    }

    private static Object command(Object wifi, JoinAttempt attempt, JSONObject request) throws Exception {
        switch (request.getString("op")) {
            case "status": return observe(wifi).result();
            case "scan": return scan(wifi);
            case "prepare": return attempt.prepare(request.getString("ssid"), request.getString("password"));
            case "associate": return attempt.associate();
            case "restore": return attempt.restore();
            case "forget": return forget(wifi, request.getInt("networkId"));
            default: return JSONObject.NULL;
        }
    }

    private static Object service(String name, String type) throws Exception {
        Object binder = Class.forName("android.os.ServiceManager")
            .getMethod("getService", String.class).invoke(null, name);
        return Class.forName(type + "$Stub").getMethod("asInterface", Class.forName("android.os.IBinder"))
            .invoke(null, binder);
    }

    private static Object call(Object service, String name, Class<?>[] types, Object... args) throws Exception {
        java.lang.reflect.Method method = service.getClass().getMethod(name, types);
        method.setAccessible(true);
        return method.invoke(service, args);
    }

    private static Link observe(Object wifi) throws Exception {
        WifiInfo info = (WifiInfo) call(wifi, "getConnectionInfo", new Class<?>[]{String.class}, PACKAGE);
        if (info == null || info.getSupplicantState() != SupplicantState.COMPLETED || info.getNetworkId() < 0) {
            return new Link(-1, null, false);
        }
        String name = ssid(info.getSSID());
        return new Link(info.getNetworkId(), name, name != null && usableRoute());
    }

    private static final class Link {
        final int id;
        final String ssid;
        final boolean reachable;

        Link(int id, String ssid, boolean reachable) {
            this.id = id;
            this.ssid = ssid;
            this.reachable = reachable;
        }

        JSONObject result() throws Exception {
            return new JSONObject().put("ssid", ssid == null ? JSONObject.NULL : ssid).put("reachable", reachable);
        }

        boolean matches(Link other) {
            return id == other.id && Objects.equals(ssid, other.ssid) && reachable == other.reachable;
        }
    }

    private static boolean usableRoute() throws Exception {
        Object connectivity = service("connectivity", "android.net.IConnectivityManager");
        LinkProperties link = (LinkProperties) call(connectivity, "getLinkPropertiesForType",
            new Class<?>[]{int.class}, ConnectivityManager.TYPE_WIFI);
        if (link == null) return false;
        for (RouteInfo route : link.getRoutes()) {
            InetAddress gateway = route.getGateway();
            if (!route.isDefaultRoute() || gateway == null || gateway.isAnyLocalAddress()) continue;
            for (LinkAddress addr : link.getLinkAddresses()) {
                if (addr.getAddress().getClass() == gateway.getClass()
                    && !addr.getAddress().isLinkLocalAddress()
                    && !addr.getAddress().isLoopbackAddress()
                    && !addr.getAddress().isAnyLocalAddress()) return true;
            }
        }
        return false;
    }

    private static JSONObject scan(Object wifi) throws Exception {
        long requested = SystemClock.elapsedRealtime();
        boolean started = (Boolean) call(wifi, "startScan", new Class<?>[]{String.class}, PACKAGE);
        List<?> results = scanResults(wifi, requested, started);
        Map<String, Integer> saved = saved(wifi);
        JSONArray rows = new JSONArray();
        for (Object item : results) {
            ScanResult result = (ScanResult) item;
            if (result.SSID == null || result.SSID.isEmpty()) continue;
            Integer id = saved.get(result.SSID);
            rows.put(new JSONObject().put("ssid", result.SSID).put("signalDbm", result.level)
                .put("secured", secured(result.capabilities)).put("networkId", id == null ? JSONObject.NULL : id));
            if (rows.length() == 512) break;
        }
        return new JSONObject().put("networks", rows);
    }

    private static List<?> scanResults(Object wifi, long requested, boolean started) throws Exception {
        while (true) {
            List<?> results = (List<?>) call(wifi, "getScanResults", new Class<?>[]{String.class}, PACKAGE);
            if (!started || SystemClock.elapsedRealtime() >= requested + 3500) return results;
            for (Object item : results) {
                if (((ScanResult) item).timestamp / 1000 >= requested) return results;
            }
            SystemClock.sleep(200);
        }
    }

    private static List<?> profiles(Object wifi, boolean credentials) throws Exception {
        String method = credentials ? "getPrivilegedConfiguredNetworks" : "getConfiguredNetworks";
        Object result = call(wifi, method, new Class<?>[0]);
        if (result instanceof List) return (List<?>) result;
        return (List<?>) call(result, "getList", new Class<?>[0]);
    }

    private static Map<String, Integer> saved(Object wifi) throws Exception {
        Map<String, Integer> ids = new HashMap<>();
        for (Object item : profiles(wifi, false)) {
            WifiConfiguration config = (WifiConfiguration) item;
            String name = ssid(config.SSID);
            if (name != null && config.networkId >= 0) ids.putIfAbsent(name, config.networkId);
        }
        return ids;
    }

    private static String ssid(String name) {
        if (name == null || "<unknown ssid>".equals(name)) return null;
        if (name.length() >= 2 && name.startsWith("\"") && name.endsWith("\"")) {
            return name.substring(1, name.length() - 1);
        }
        if (name.matches("(?:[0-9a-fA-F]{2}){1,32}")) {
            byte[] bytes = new byte[name.length() / 2];
            for (int i = 0; i < bytes.length; i++) {
                bytes[i] = (byte) Integer.parseInt(name.substring(i * 2, i * 2 + 2), 16);
            }
            return new String(bytes, StandardCharsets.UTF_8);
        }
        return name;
    }

    private static boolean secured(String capabilities) {
        return capabilities != null && (capabilities.contains("WEP") || capabilities.contains("WPA")
            || capabilities.contains("PSK") || capabilities.contains("SAE") || capabilities.contains("EAP")
            || capabilities.contains("OWE"));
    }

    private static WifiConfiguration profile(Object wifi, int id, boolean credentials) throws Exception {
        for (Object item : profiles(wifi, credentials)) {
            WifiConfiguration config = (WifiConfiguration) item;
            if (config.networkId == id) return config;
        }
        return null;
    }

    private static boolean remove(Object wifi, int id) throws Exception {
        return (Boolean) call(wifi, "removeNetwork", new Class<?>[]{int.class, String.class}, id, PACKAGE);
    }

    private static Object forget(Object wifi, int id) throws Exception {
        if (profile(wifi, id, false) == null) return "not-saved";
        if (!remove(wifi, id) || profile(wifi, id, false) != null) return JSONObject.NULL;
        Link actual = waitFor(wifi, 5000, link -> link.id != id);
        return actual == null ? JSONObject.NULL : actual.result();
    }

    private static Link waitFor(Object wifi, long millis, Predicate<Link> matches) throws Exception {
        long deadline = SystemClock.elapsedRealtime() + millis;
        do {
            Link link = observe(wifi);
            if (matches.test(link)) return link;
            SystemClock.sleep(200);
        } while (SystemClock.elapsedRealtime() < deadline);
        return null;
    }

    private static final class JoinAttempt {
        private final Object wifi;
        private Link previous;
        private WifiConfiguration saved;
        private WifiConfiguration candidate;
        private String target;
        private int candidateId = -1;
        private boolean triedUpdate;
        private boolean selected;

        JoinAttempt(Object wifi) {
            this.wifi = wifi;
        }

        boolean prepare(String ssid, String password) throws Exception {
            target = ssid;
            previous = observe(wifi);
            if (previous.id >= 0 && profile(wifi, previous.id, false) == null) return false;
            boolean open = password.isEmpty();
            int key = open ? WifiConfiguration.KeyMgmt.NONE : WifiConfiguration.KeyMgmt.WPA_PSK;
            WifiConfiguration existing = matchingProfile(ssid, key);
            if (existing != null) {
                saved = open ? existing : profile(wifi, existing.networkId, true);
                if (saved == null || (!open && (saved.preSharedKey == null || "*".equals(saved.preSharedKey)))) {
                    return false;
                }
                candidate = WifiConfiguration.class.getConstructor(WifiConfiguration.class).newInstance(saved);
            } else {
                candidate = new WifiConfiguration();
                StringBuilder hex = new StringBuilder();
                for (byte value : ssid.getBytes(StandardCharsets.UTF_8)) {
                    hex.append(Character.forDigit((value & 0xff) >> 4, 16));
                    hex.append(Character.forDigit(value & 0xf, 16));
                }
                candidate.SSID = hex.toString();
                candidate.hiddenSSID = true;
                candidate.allowedKeyManagement.set(key);
            }
            if (!open) {
                candidate.preSharedKey = password.matches("[0-9a-fA-F]{64}") ? password : "\"" + password + "\"";
            }
            return true;
        }

        private WifiConfiguration matchingProfile(String ssid, int key) throws Exception {
            for (Object item : profiles(wifi, false)) {
                WifiConfiguration config = (WifiConfiguration) item;
                if (ssid.equals(ssid(config.SSID)) && config.allowedKeyManagement.get(key)
                    && (key != WifiConfiguration.KeyMgmt.NONE || !hasWepKey(config))) return config;
            }
            return null;
        }

        private boolean hasWepKey(WifiConfiguration profile) {
            if (profile.wepKeys != null) {
                for (String key : profile.wepKeys) {
                    if (key != null) return true;
                }
            }
            return false;
        }

        Object associate() throws Exception {
            if (candidate == null) return JSONObject.NULL;
            triedUpdate = true;
            candidateId = update(candidate);
            if (candidateId < 0) return JSONObject.NULL;
            selected = true;
            if (!disconnect() || !select(candidateId)) return JSONObject.NULL;
            Link actual = waitFor(wifi, 25000, link ->
                link.id == candidateId && target.equals(link.ssid) && link.reachable);
            return actual == null ? JSONObject.NULL : actual.result();
        }

        Object restore() throws Exception {
            if (!triedUpdate) return JSONObject.NULL;
            boolean restored;
            try {
                restored = restoreProfile();
            } catch (Exception e) {
                restored = false;
            }
            // Reselect the previous profile once the failed candidate is gone.
            if (selected && previous.id >= 0) {
                restored = disconnect() && select(previous.id) && restored;
            } else if (selected && previous.id < 0) {
                restored = disconnect() && restored;
            }
            Link actual = restored ? waitFor(wifi, 20000, previous::matches) : null;
            return actual == null ? JSONObject.NULL : actual.result();
        }

        private boolean restoreProfile() throws Exception {
            if (saved == null) {
                if (candidateId >= 0 && !remove(wifi, candidateId)) return false;
                return matchingProfile(target, candidate.allowedKeyManagement.nextSetBit(0)) == null;
            }
            if (update(saved) != saved.networkId) return false;
            boolean disabled = saved.status == WifiConfiguration.Status.DISABLED;
            if (disabled && !(Boolean) call(wifi, "disableNetwork", new Class<?>[]{int.class, String.class},
                saved.networkId, PACKAGE)) return false;
            WifiConfiguration observed = profile(wifi, saved.networkId, saved.preSharedKey != null);
            return observed != null && Objects.equals(saved.preSharedKey, observed.preSharedKey)
                && disabled == (observed.status == WifiConfiguration.Status.DISABLED);
        }

        private int update(WifiConfiguration config) throws Exception {
            return (Integer) call(wifi, "addOrUpdateNetwork",
                new Class<?>[]{WifiConfiguration.class, String.class}, config, PACKAGE);
        }

        private boolean disconnect() throws Exception {
            call(wifi, "disconnect", new Class<?>[]{String.class}, PACKAGE);
            return waitFor(wifi, 3000, link -> link.id < 0 && clearedRoute()) != null;
        }

        private boolean clearedRoute() {
            try {
                return !usableRoute();
            } catch (Exception e) {
                return false;
            }
        }

        private boolean select(int id) throws Exception {
            if (!(Boolean) call(wifi, "enableNetwork", new Class<?>[]{int.class, boolean.class, String.class},
                id, true, PACKAGE)) return false;
            call(wifi, "reconnect", new Class<?>[]{String.class}, PACKAGE);
            return true;
        }
    }
}
