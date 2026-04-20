// SVG recreation of SuperHQ's desktop pairing-approval modal — same
// layout and tokens as the real thing (see src/main.rs
// render_pairing_modal). The only non-static bit is the soft pulse on
// the Approve button to hint where the user should click.

interface Props {
    className?: string;
    deviceLabel?: string;
}

export function PairingModalIllustration({
    className = "",
    deviceLabel = "MacIntel",
}: Props) {
    return (
        <svg
            viewBox="0 0 400 230"
            className={className}
            aria-hidden="true"
        >
            {/* dimmed window backdrop */}
            <rect x="0" y="0" width="400" height="230" fill="#0f0f0f" />

            {/* modal card — #1a1a1a base, subtle white border like desktop */}
            <rect
                x="20"
                y="20"
                width="360"
                height="190"
                rx="10"
                fill="#1a1a1a"
                stroke="rgba(255,255,255,0.07)"
            />

            {/* title */}
            <text
                x="40"
                y="52"
                fontSize="13"
                fontWeight="500"
                fill="rgba(255,255,255,0.93)"
                fontFamily="ui-sans-serif, system-ui, sans-serif"
            >
                Allow this device to connect?
            </text>

            {/* description — two lines */}
            <text
                x="40"
                y="74"
                fontSize="11"
                fill="rgba(255,255,255,0.6)"
                fontFamily="ui-sans-serif, system-ui, sans-serif"
            >
                A remote client is requesting pairing with this host.
            </text>
            <text
                x="40"
                y="89"
                fontSize="11"
                fill="rgba(255,255,255,0.6)"
                fontFamily="ui-sans-serif, system-ui, sans-serif"
            >
                Approve only if you initiated it on a device you control.
            </text>

            {/* Device label card */}
            <rect
                x="40"
                y="104"
                width="320"
                height="46"
                rx="6"
                fill="rgba(255,255,255,0.023)"
            />
            <text
                x="52"
                y="122"
                fontSize="10"
                fill="rgba(255,255,255,0.4)"
                fontFamily="ui-sans-serif, system-ui, sans-serif"
            >
                Device label
            </text>
            <text
                x="52"
                y="140"
                fontSize="11"
                fontFamily="ui-monospace, Menlo, monospace"
                fill="rgba(255,255,255,0.73)"
            >
                {deviceLabel}
            </text>

            {/* Reject button */}
            <rect
                x="236"
                y="168"
                width="60"
                height="28"
                rx="6"
                fill="rgba(255,255,255,0.023)"
                stroke="rgba(255,255,255,0.07)"
            />
            <text
                x="266"
                y="185.5"
                textAnchor="middle"
                fontSize="11"
                fill="rgba(255,255,255,0.73)"
                fontFamily="ui-sans-serif, system-ui, sans-serif"
            >
                Reject
            </text>

            {/* Approve button with soft pulse */}
            <rect
                x="302"
                y="168"
                width="68"
                height="28"
                rx="6"
                fill="#7b9ef0"
            >
                <animate
                    attributeName="opacity"
                    values="0.85;1;0.85"
                    dur="1.6s"
                    repeatCount="indefinite"
                />
            </rect>
            <text
                x="336"
                y="185.5"
                textAnchor="middle"
                fontSize="11"
                fontWeight="500"
                fill="#ffffff"
                fontFamily="ui-sans-serif, system-ui, sans-serif"
            >
                Approve
            </text>
            {/* glow ring */}
            <rect
                x="302"
                y="168"
                width="68"
                height="28"
                rx="6"
                fill="none"
                stroke="#7b9ef0"
                strokeOpacity="0.35"
            >
                <animate
                    attributeName="stroke-width"
                    values="0;6;0"
                    dur="1.6s"
                    repeatCount="indefinite"
                />
                <animate
                    attributeName="stroke-opacity"
                    values="0.4;0;0.4"
                    dur="1.6s"
                    repeatCount="indefinite"
                />
            </rect>
        </svg>
    );
}

// Touch ID / Face ID indicator — minimal, sober, not the pulsing-glow
// spectacle we had earlier.
export function BiometricIllustration({ className = "" }: { className?: string }) {
    return (
        <svg viewBox="0 0 160 160" className={className} aria-hidden="true">
            {/* subtle outer ring */}
            <circle
                cx="80"
                cy="80"
                r="62"
                fill="none"
                stroke="rgba(255,255,255,0.08)"
                strokeWidth="1"
            />
            <circle
                cx="80"
                cy="80"
                r="54"
                fill="none"
                stroke="#7b9ef0"
                strokeOpacity="0.25"
                strokeWidth="2"
            >
                <animate
                    attributeName="r"
                    values="50;58;50"
                    dur="2s"
                    repeatCount="indefinite"
                />
                <animate
                    attributeName="stroke-opacity"
                    values="0.35;0.05;0.35"
                    dur="2s"
                    repeatCount="indefinite"
                />
            </circle>

            <g
                transform="translate(54 48)"
                stroke="rgba(255,255,255,0.93)"
                fill="none"
                strokeWidth="2"
                strokeLinecap="round"
            >
                <path d="M10 58 C 10 32 22 22 36 22 C 50 22 62 32 62 58" />
                <path d="M18 58 C 18 36 26 30 36 30 C 46 30 54 36 54 58" />
                <path d="M26 58 C 26 42 30 38 36 38 C 42 38 46 42 46 58" />
                <path d="M34 58 C 34 50 36 48 36 48 C 36 48 38 50 38 58" />
                <path
                    d="M6 48 C 10 28 22 14 36 14 C 50 14 62 28 66 48"
                    opacity="0.55"
                />
            </g>
        </svg>
    );
}
