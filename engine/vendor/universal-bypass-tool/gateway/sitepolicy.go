package gateway

import (
	"net"
	"strings"
)

type SiteSplitMode int

const (
	SiteSplitOff SiteSplitMode = iota
	SiteSplitExclude
	SiteSplitInclude
)

func ParseSiteSplitMode(name string) SiteSplitMode {
	switch name {
	case "exclude":
		return SiteSplitExclude
	case "include":
		return SiteSplitInclude
	default:
		return SiteSplitOff
	}
}

// SitePolicy matches domains (normalized, with wildcard-suffix support like "*.ru") and literal IPs against a destination to decide whether it bypasses the tunnel.
type SitePolicy struct {
	mode  SiteSplitMode
	sites []string
	// ips holds literal destination addresses from the list.
	ips map[string]struct{}
}

func NewSitePolicy(mode SiteSplitMode, sites []string) *SitePolicy {
	p := &SitePolicy{mode: mode, ips: make(map[string]struct{}, len(sites))}
	seen := make(map[string]struct{}, len(sites))
	for _, s := range sites {
		s = strings.TrimSpace(strings.ToLower(strings.TrimSuffix(s, ".")))
		if s == "" {
			continue
		}
		if ip := net.ParseIP(strings.Trim(strings.TrimSpace(s), "[]")); ip != nil {
			if _, dup := p.ips[ip.String()]; !dup {
				p.ips[ip.String()] = struct{}{}
			}
			continue
		}
		s = strings.TrimPrefix(s, "*.")
		s = strings.TrimPrefix(s, ".")
		if s == "" || strings.ContainsAny(s, " /") {
			continue
		}
		if _, dup := seen[s]; dup {
			continue
		}
		seen[s] = struct{}{}
		p.sites = append(p.sites, s)
	}
	return p
}

func (p *SitePolicy) Enabled() bool {
	return p != nil && p.mode != SiteSplitOff && (len(p.sites) > 0 || len(p.ips) > 0)
}

func (p *SitePolicy) matched(domain string) bool {
	for _, s := range p.sites {
		if domain == s || strings.HasSuffix(domain, "."+s) {
			return true
		}
	}
	return false
}

func (p *SitePolicy) matchedIP(host string) bool {
	if host == "" {
		return false
	}
	ip := net.ParseIP(strings.Trim(host, "[]"))
	if ip == nil {
		return false
	}
	_, ok := p.ips[ip.String()]
	return ok
}

func (p *SitePolicy) ShouldBypass(domain string) bool {
	switch p.mode {
	case SiteSplitExclude:
		return p.matched(normalizeDomain(domain))
	case SiteSplitInclude:
		return !p.matched(normalizeDomain(domain))
	}
	return false
}

// ShouldBypassDest: when several candidate names share an IP and only some are listed, any match wins so a blocked site can never slip past by sharing an address with an unlisted one.
func (p *SitePolicy) ShouldBypassDest(ip string, domains []string) bool {
	ipMatch := p.matchedIP(ip)
	switch p.mode {
	case SiteSplitExclude:
		if ipMatch {
			return true
		}
		return p.domainMatch(domains)
	case SiteSplitInclude:
		if ipMatch || p.domainMatch(domains) {
			return false
		}
		return true
	}
	return false
}

func (p *SitePolicy) domainMatch(domains []string) bool {
	for _, d := range domains {
		if p.matched(normalizeDomain(d)) {
			return true
		}
	}
	return false
}

func normalizeDomain(domain string) string {
	return strings.TrimSuffix(strings.ToLower(strings.TrimSpace(domain)), ".")
}
