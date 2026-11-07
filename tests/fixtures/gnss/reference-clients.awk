# `chronyc -n clients` to the shape of ChroGPS Dash's ?ajax=clients reply.
# Columns: Hostname NTP Drop Int IntL Last Cmd Drop Int Last
BEGIN { printf "{\"clients\":["; n = 0 }
/^Hostname|^====/ { next }
NF >= 6 && $6 != "-" {
	t = $6
	s = t + 0
	if (t ~ /m$/) s *= 60
	else if (t ~ /h$/) s *= 3600
	else if (t ~ /d$/) s *= 86400
	else if (t ~ /y$/) s *= 31536000
	printf "%s{\"ip\":\"%s\",\"ntp_hits\":\"%s\",\"ntp_drops\":\"%s\",\"last_ntp\":\"%s\",\"last_ntp_seconds\":%d}", (n++ ? "," : ""), $1, $2, $3, t, s
}
END { printf "]}\n" }
