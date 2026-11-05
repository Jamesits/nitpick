import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

import nitpick

ApplicationWindow {
    id: root

    readonly property var patterns: JSON.parse(launcher.catalogue())
    readonly property var pattern: patterns[patternList.currentIndex]
    readonly property var encoding: pattern.encodings[Math.max(0, encodingBox.currentIndex)]

    width: 1100
    height: 760
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
                text: qsTr("Display")
                font.bold: true
            }

            ComboBox {
                id: screenBox

                Layout.fillWidth: true
                model: Qt.application.screens
                displayText: currentIndex >= 0 ? textAt(currentIndex) : ""
                delegate: ItemDelegate {
                    required property var modelData
                    width: ListView.view.width
                    text: modelData.name + " - " + modelData.manufacturer + " " + modelData.model
                }

                function textAt(i) {
                    const s = Qt.application.screens[i];
                    return s.name + " - " + s.manufacturer + " " + s.model;
                }
            }

            Button {
                Layout.fillWidth: true
                text: qsTr("Show pattern (Esc closes)")
                onClicked: launcher.showPattern(root.pattern.id, root.encoding.id, screenBox.currentIndex)
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
                text: qsTr("Before measuring: turn on Windows HDR for the display, set the GPU output to 10 bpc. " +
                           "Narrow-range codes reach the wire unchanged only with RGB full-range output. " +
                           "See doc/design/test_patterns.md.")
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
                    text: launcher.referenceTable(root.pattern.id, root.encoding.id)
                }
            }
        }
    }
}
