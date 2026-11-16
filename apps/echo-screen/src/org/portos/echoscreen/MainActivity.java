package org.portos.echoscreen;

import android.app.Activity;
import android.content.Intent;
import android.content.SharedPreferences;
import android.graphics.Color;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.Gravity;
import android.view.View;
import android.view.WindowManager;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.FrameLayout;
import android.widget.TextView;

/** Thin Android adapters. No URL, intent, error description or token is logged. */
public final class MainActivity extends Activity implements ScreenLoad.Port {
    private final Handler handler = new Handler(Looper.getMainLooper());
    private ScreenLoad load;
    private FrameLayout frame;
    private TextView message;
    private WebView web;

    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
        frame = new FrameLayout(this);
        frame.setBackgroundColor(Color.BLACK);
        // Raw physical pixels: hassmic's grip and light band overlay the app.
        frame.setPadding(0, ScreenConfig.TOP, 0, ScreenConfig.BOTTOM);
        message = new TextView(this);
        message.setTextColor(Color.WHITE);
        message.setBackgroundColor(Color.BLACK);
        message.setGravity(Gravity.CENTER);
        frame.addView(message, fill());
        setContentView(frame);
        load = new ScreenLoad(this);
        configure(getIntent());
        immersive();
    }

    @Override protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        configure(intent);
    }

    private void configure(Intent intent) {
        SharedPreferences preferences = getSharedPreferences("echo-screen", MODE_PRIVATE);
        ScreenConfig.Store store = new ScreenConfig.Store() {
            public Secret read() {
                String value = preferences.getString("url", null);
                return value == null ? null : new Secret(value);
            }
            public void save(Secret url) {
                preferences.edit().putString("url", url.expose()).apply();
            }
        };
        String extra = intent.getStringExtra("url");
        load.start(ScreenConfig.select(intent.hasExtra("url"),
                extra == null ? null : new Secret(extra), store));
    }

    private FrameLayout.LayoutParams fill() {
        return new FrameLayout.LayoutParams(-1, -1);
    }

    private void immersive() {
        getWindow().getDecorView().setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY | View.SYSTEM_UI_FLAG_FULLSCREEN
                | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION | View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
    }

    @Override public void onWindowFocusChanged(boolean focused) {
        super.onWindowFocusChanged(focused);
        if (focused) immersive();
    }

    @Override public void cancel() {
        handler.removeCallbacksAndMessages(null);
        if (web != null) {
            web.stopLoading();
            frame.removeView(web);
            web.destroy();
            web = null;
        }
    }

    @Override public void waiting(String text) {
        message.setText(text);
        message.setVisibility(View.VISIBLE);
        if (web != null) web.setVisibility(View.INVISIBLE);
        message.bringToFront();
    }

    @Override public void load(Secret url, long attempt) {
        // A fresh view per attempt isolates late Android callbacks from older loads.
        web = new WebView(this);
        web.setVisibility(View.INVISIBLE);
        web.getSettings().setJavaScriptEnabled(true);
        web.getSettings().setDomStorageEnabled(true);
        web.setWebViewClient(new WebViewClient() {
            @Override public void onReceivedError(WebView view, WebResourceRequest request,
                    WebResourceError error) {
                load.error(attempt, request.isForMainFrame());
            }
            @Override public void onReceivedHttpError(WebView view, WebResourceRequest request,
                    WebResourceResponse response) {
                load.error(attempt, request.isForMainFrame());
            }
            @Override public void onPageFinished(WebView view, String ignored) {
                load.finished(attempt);
            }
        });
        frame.addView(web, fill());
        message.bringToFront();
        web.loadUrl(url.expose());
    }

    @Override public void schedule(long delay, Runnable retry) {
        handler.postDelayed(retry, delay);
    }

    @Override public void showPage() {
        message.setVisibility(View.GONE);
        if (web != null) web.setVisibility(View.VISIBLE);
    }

    @Override protected void onDestroy() {
        if (load != null) load.destroy();
        super.onDestroy();
    }
}
