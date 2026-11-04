package io.github.ssk101.tabletedge

import android.app.Activity
import android.content.Intent
import android.graphics.Typeface
import android.os.Bundle
import android.provider.Settings
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

class MainActivity : Activity() {

    private lateinit var logView: TextView
    private lateinit var logScroll: ScrollView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val pad = (16 * resources.displayMetrics.density).toInt()

        val hint = TextView(this).apply {
            text = "Turn on Tablet Edge under Accessibility, then start tablet-edge on the PC."
        }
        val accessibilityButton = Button(this).apply {
            text = "Accessibility settings"
            setOnClickListener { startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS)) }
        }
        logView = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            setTextIsSelectable(true)
        }
        logScroll = ScrollView(this).apply { addView(logView) }

        setContentView(LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad, pad, pad)
            // Keep content clear of the system bars (edge-to-edge is forced from Android 15).
            setOnApplyWindowInsetsListener { v, insets ->
                val bars = insets.getInsets(android.view.WindowInsets.Type.systemBars())
                v.setPadding(pad + bars.left, pad + bars.top, pad + bars.right, pad + bars.bottom)
                insets
            }
            addView(hint)
            addView(accessibilityButton)
            addView(logScroll, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
        })
    }

    override fun onResume() {
        super.onResume()
        EventLog.listener = ::render
        render()
    }

    override fun onPause() {
        EventLog.listener = null
        super.onPause()
    }

    private fun render() {
        logView.text = EventLog.text()
        logScroll.post { logScroll.fullScroll(ScrollView.FOCUS_DOWN) }
    }
}
