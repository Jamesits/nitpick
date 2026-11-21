import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

import nitpick

ApplicationWindow {
    id: root

    readonly property var catalogue: JSON.parse(launcher.catalogue())
    readonly property var patterns: catalogue.patterns
    readonly property var pattern: patterns[patternList.currentIndex]
    readonly property var encoding: pattern.encodings[Math.max(0, encodingBox.currentIndex)]
    readonly property var depth: catalogue.depths[Math.max(0, depthBox.currentIndex)]
    readonly property var drivers: [
        { id: "qrhi", label: qsTr("Local screen (QRhi window)") },
        { id: "browser", label: qsTr("Browser (local or remote)") },
        { id: "webview", label: qsTr("Browser (local webview)") }
    ]
    readonly property string driverId: drivers[Math.max(0, driverBox.currentIndex)].id
    // The browser outputs use the browser service and its PNG depths.
    readonly property bool browserOutput: driverId === "browser" || driverId === "webview"
    // `[{driver, key, name, width, height}]` of all drivers, see launcher.rs.
    readonly property var allDisplays: JSON.parse(launcher.displays || "[]")
    // The local webview opens on a QRhi screen.
    readonly property var displays: allDisplays.filter(
        d => d.driver === (driverId === "webview" ? "qrhi" : driverId))
    // Key of the selected display: the native screen name or the browser
    // client ID. It survives a change of the list, unlike the list index.
    property string selectedKey: ""
    // Interface addresses for remote displays, read when the browser
    // output is first selected and at "Restart service".
    property var addresses: []

    // Keeps the selected display when the list changes. Called later, so
    // that the combo box has the new model first.
    onDisplaysChanged: Qt.callLater(() => {
        const i = displays.findIndex(d => d.key === selectedKey);
        screenBox.currentIndex = displays.length > 0 ? Math.max(0, i) : -1;
        selectedKey = displays.length > 0 ? displays[screenBox.currentIndex].key : "";
    })
    Component.onCompleted: launcher.refreshDisplays()

    // Starts the browser service, or starts it again with the settings of
    // the browser panel. Remote displays are off at each launcher start.
    function startBrowser() {
        launcher.startBrowser(remoteCheck.checked, addressBox.currentText, portBox.value);
    }

    function readAddresses() {
        addresses = JSON.parse(launcher.interfaceAddresses());
        addressBox.currentIndex = Math.max(0, addresses.indexOf(launcher.browserAddress));
    }

    // Screens added or removed: a display change.
    Connections {
        target: Qt.application
        function onScreensChanged() {
            launcher.refreshDisplays();
        }
    }

    // Launcher JSON for the HDR10 static metadata, see launcher.rs.
    function metadataJson() {
        return JSON.stringify({
            enabled: metadataCheck.checked,
            primaries: catalogue.primaries[Math.max(0, primariesBox.currentIndex)].id,
            maxMastering: Number(maxMasteringField.text),
            minMastering: Number(minMasteringField.text),
            fromPattern: fromPatternCheck.checked,
            maxCll: Number(maxCllField.text),
            maxFall: Number(maxFallField.text)
        });
    }

    width: 1100
    height: 860
    visible: true
    title: qsTr("nitpick - HDR/WCG test patterns")

    // The pattern window is a separate top-level window; it must not keep the
    // application alive after the launcher closes.
    onClosing: Qt.quit()

    Launcher {
        id: launcher
    }

    RowLayout {
        anchors.fill: parent
        anchors.margins: 12
        spacing: 12

        ColumnLayout {
            Layout.preferredWidth: 380
            Layout.fillHeight: true
            spacing: 8

            Label {
                text: qsTr("Pattern")
                font.bold: true
            }

            ListView {
                id: patternList

                Layout.fillWidth: true
                Layout.preferredHeight: contentHeight
                interactive: false
                model: root.patterns
                delegate: ItemDelegate {
                    required property var modelData
                    required property int index

                    width: ListView.view.width
                    highlighted: ListView.isCurrentItem
                    onClicked: patternList.currentIndex = index
                    contentItem: ColumnLayout {
                        Label {
                            text: modelData.label
                            font.bold: true
                        }
                        Label {
                            Layout.fillWidth: true
                            text: modelData.description
                            wrapMode: Text.WordWrap
                            opacity: 0.8
                        }
                    }
                }
            }

            Label {
                text: qsTr("Signal encoding")
                font.bold: true
            }

            ComboBox {
                id: encodingBox

                Layout.fillWidth: true
                model: root.pattern.encodings
                textRole: "label"
            }

            Label {
                text: qsTr("Bit depth")
                font.bold: true
            }

            ComboBox {
                id: depthBox

                Layout.fillWidth: true
                model: root.catalogue.depths
                textRole: root.browserOutput ? "browserLabel" : "label"
            }

            Label {
                text: qsTr("Output")
                font.bold: true
            }

            ComboBox {
                id: driverBox

                Layout.fillWidth: true
                model: root.drivers
                textRole: "label"
                // A display change. The browser service starts at its
                // first selection and runs until the launcher ends.
                onActivated: {
                    if (root.browserOutput && launcher.browserUrl === "") {
                        root.readAddresses();
                        root.startBrowser();
                    }
                    launcher.refreshDisplays();
                }
            }

            GridLayout {
                Layout.fillWidth: true
                columns: 2
                visible: root.browserOutput

                CheckBox {
                    id: remoteCheck

                    Layout.columnSpan: 2
                    text: qsTr("Allow remote displays on this network")
                    enabled: root.addresses.length > 0
                    onToggled: root.startBrowser()
                }
                Label {
                    text: qsTr("Interface address")
                }
                ComboBox {
                    id: addressBox

                    Layout.fillWidth: true
                    model: root.addresses
                    enabled: remoteCheck.checked
                    onActivated: root.startBrowser()
                }
                Label {
                    text: qsTr("Port (0: any)")
                }
                RowLayout {
                    Layout.fillWidth: true

                    SpinBox {
                        id: portBox

                        Layout.fillWidth: true
                        from: 0
                        to: 65535
                        editable: true
                        value: launcher.browserPort
                        textFromValue: (v, locale) => String(v)
                        valueFromText: (text, locale) => Number(text)
                    }
                    Button {
                        text: qsTr("Restart service")
                        // Also after a network change, which can change
                        // the interface addresses.
                        onClicked: {
                            root.readAddresses();
                            root.startBrowser();
                        }
                    }
                }
                Label {
                    text: qsTr("URL")
                }
                TextField {
                    Layout.fillWidth: true
                    readOnly: true
                    selectByMouse: true
                    text: launcher.browserUrl
                    placeholderText: qsTr("not running")
                }
                Label {
                    text: qsTr("Pairing code")
                }
                Label {
                    text: launcher.pairingCode || "-"
                    font.bold: true
                    font.pixelSize: 24
                }
                Label {
                    Layout.columnSpan: 2
                    Layout.fillWidth: true
                    text: launcher.browserClient
                    wrapMode: Text.WordWrap
                }
            }

            Label {
                text: qsTr("Display")
                font.bold: true
            }

            ComboBox {
                id: screenBox

                Layout.fillWidth: true
                model: root.displays
                textRole: "name"
                // A display change cancels the pending presentation.
                onActivated: index => {
                    if (root.displays[index].key !== root.selectedKey) {
                        root.selectedKey = root.displays[index].key;
                        launcher.refreshDisplays();
                    }
                }
            }

            CheckBox {
                id: metadataCheck

                text: qsTr("Send HDR10 static metadata")
            }

            GridLayout {
                Layout.fillWidth: true
                columns: 2
                enabled: metadataCheck.checked

                Label {
                    text: qsTr("Mastering primaries")
                }
                ComboBox {
                    id: primariesBox

                    Layout.fillWidth: true
                    model: root.catalogue.primaries
                    textRole: "label"
                }
                Label {
                    text: qsTr("Mastering max (cd/m2)")
                }
                TextField {
                    id: maxMasteringField

                    Layout.fillWidth: true
                    text: "1000"
                    validator: DoubleValidator {
                        bottom: 0
                        top: 10000
                    }
                }
                Label {
                    text: qsTr("Mastering min (cd/m2)")
                }
                TextField {
                    id: minMasteringField

                    Layout.fillWidth: true
                    text: "0.0001"
                    validator: DoubleValidator {
                        bottom: 0
                        top: 10000
                    }
                }
                CheckBox {
                    id: fromPatternCheck

                    Layout.columnSpan: 2
                    text: qsTr("MaxCLL / MaxFALL from the pattern")
                    checked: true
                }
                Label {
                    text: qsTr("MaxCLL (cd/m2)")
                }
                TextField {
                    id: maxCllField

                    Layout.fillWidth: true
                    enabled: !fromPatternCheck.checked
                    text: "1000"
                    validator: IntValidator {
                        bottom: 0
                        top: 65535
                    }
                }
                Label {
                    text: qsTr("MaxFALL (cd/m2)")
                }
                TextField {
                    id: maxFallField

                    Layout.fillWidth: true
                    enabled: !fromPatternCheck.checked
                    text: "400"
                    validator: IntValidator {
                        bottom: 0
                        top: 65535
                    }
                }
            }

            RowLayout {
                Layout.fillWidth: true

                Label {
                    text: qsTr("Measurement delay (ms)")
                }
                // The value comes from the saved setting; each change is
                // saved. The upper limit is a UI choice, not a timer limit.
                SpinBox {
                    id: delayBox

                    Layout.fillWidth: true
                    from: 0
                    to: 600000
                    stepSize: 100
                    editable: true
                    value: launcher.measurementDelay
                    onValueModified: launcher.saveMeasurementDelay(value)
                }
            }

            Button {
                Layout.fillWidth: true
                text: qsTr("Show pattern (Esc closes)")
                onClicked: launcher.showPattern(root.pattern.id, root.encoding.id, root.depth.id,
                                                root.driverId, root.selectedKey, root.metadataJson(),
                                                delayBox.value)
            }

            Label {
                Layout.fillWidth: true
                text: launcher.status
                wrapMode: Text.WordWrap
                color: launcher.status.startsWith("Error") ? "firebrick" : palette.text
            }

            Label {
                Layout.fillWidth: true
                Layout.topMargin: 8
                wrapMode: Text.WordWrap
                opacity: 0.8
                text: root.driverId === "browser"
                      ? qsTr("Open the URL in the browser, enter the pairing code and select " +
                             "\"Full screen\" in the page. Then select the browser in the display list. " +
                             "See doc/design/output_driver/browser.md.")
                      : root.driverId === "webview"
                      ? qsTr("Select the screen. \"Show pattern\" opens the webview on it. Enter the " +
                             "pairing code in the webview and select \"Full screen\" in the page. " +
                             "See doc/design/output_driver/browser.md.")
                      : qsTr("Before measuring: turn on HDR for the display, set the GPU output to 10 bpc " +
                             "(12 bpc for 12-bit). Narrow-range codes reach the wire unchanged only with RGB " +
                             "full-range output. See doc/design/test_patterns.md.")
            }

            Item {
                Layout.fillHeight: true
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true

            Label {
                text: qsTr("Reference values")
                font.bold: true
            }

            ScrollView {
                Layout.fillWidth: true
                Layout.fillHeight: true

                TextArea {
                    readOnly: true
                    selectByMouse: true
                    font.family: "Consolas"
                    font.pixelSize: 13
                    // New device reports change the path limits.
                    text: {
                        launcher.deviceRevision;
                        return launcher.referenceTable(root.pattern.id, root.encoding.id,
                                                       root.depth.id, root.driverId, root.selectedKey);
                    }
                }
            }
        }
    }
}
