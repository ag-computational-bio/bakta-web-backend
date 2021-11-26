package argoclient

import (
	"fmt"
	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"strings"
)

//CreateBaktaConfString Creates a bakta config string based on the configuration and job settings provided
func CreateBaktaConfString(conf *api.JobConfig) (string, error) {
	var confStringElements []string

	//confStringElements = append(confStringElements, "bakta")
	//
	//confStringElements = append(confStringElements, "--tmp-dir", "/cache")
	//confStringElements = append(confStringElements, "--threads", "12")
	//confStringElements = append(confStringElements, "--prefix", "result")
	//confStringElements = append(confStringElements, "-o", "/output")
	//confStringElements = append(confStringElements, "--db", "/db/db")

	if conf.HasProdigal {
		confStringElements = append(confStringElements, "--prodigal-tf", "/data/prodigaltraining.tf")
	}

	if conf.HasReplicons {
		confStringElements = append(confStringElements, "--replicons", "/data/replicons.tsv")
	}

	if conf.CompleteGenome {
		confStringElements = append(confStringElements, "--complete")
	}

	if conf.Locus != "" {
		confStringElements = append(confStringElements, fmt.Sprintf("--locus %v", conf.Locus))
	}

	if conf.LocusTag != "" {
		confStringElements = append(confStringElements, fmt.Sprintf("--locus-tag %v", conf.LocusTag))
	}

	if conf.KeepContigHeaders {
		confStringElements = append(confStringElements, "--keep-contig-headers")
	}

	if conf.Genus != "" {
		confStringElements = append(confStringElements, fmt.Sprintf("--genus '%v'", conf.Genus))
	}

	if conf.Species != "" {
		confStringElements = append(confStringElements, fmt.Sprintf("--species '%v'", conf.Species))
	}

	if conf.Strain != "" {
		confStringElements = append(confStringElements, fmt.Sprintf("--strain '%v'", conf.Strain))
	}

	if conf.Plasmid != "" {
		confStringElements = append(confStringElements, fmt.Sprintf("--plasmid '%v'", conf.Plasmid))
	}

	if conf.Compliant {
		confStringElements = append(confStringElements, fmt.Sprintf("--compliant"))
	}

	if conf.TranslationalTable == 4 || conf.TranslationalTable == 11 {
		confStringElements = append(confStringElements, "--translation-table", fmt.Sprintf("%v", conf.TranslationalTable))
	}

	dermtype := "?"

	switch conf.DermType {
	case api.DermType_UNKNOWN:
		dermtype = "?"
	case api.DermType_MONODERM:
		dermtype = "+"
	case api.DermType_DIDERM:
		dermtype = "-"
	}

	confStringElements = append(confStringElements, "--gram", fmt.Sprintf("%s", dermtype))

	confString := strings.Join(confStringElements, " ")

	return confString, nil
}
