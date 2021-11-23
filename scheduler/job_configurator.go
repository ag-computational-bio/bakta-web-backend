package scheduler

import (
	"encoding/json"
	"fmt"
	"path"
	"reflect"
	"strings"

	"github.com/spf13/viper"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
)

//createDownloadConf Creates the configuration string for the download part of a bakta job
//The job has to be provided along with two bools that indicate if a prodigal training file and/or a replicon file are present
func createDownloadConf(job *database.Job, prodigaltf bool, replicontsv bool) (string, error) {
	keyString := job.FastaKey
	bucketString := job.DataBucket
	if prodigaltf {
		keyString = fmt.Sprintf(keyString+",%v", job.ProdigalKey)
		bucketString = fmt.Sprintf(bucketString+",%v", job.DataBucket)
	}
	if replicontsv {
		keyString = fmt.Sprintf(keyString+",%v", job.RepliconKey)
		bucketString = fmt.Sprintf(bucketString+",%v", job.DataBucket)
	}

	confString := fmt.Sprintf("download -b %v -k %v -d /data -e s3.computational.bio.uni-giessen.de", bucketString, keyString)

	return confString, nil
}

//createBaktaConf Creates a bakta config string based on the configuration and job settings provided
func createBaktaConf(job *database.Job, conf *api.JobConfig) ([]byte, error) {
	var confStringElements []string

	confStringElements = append(confStringElements, "bakta")

	confStringElements = append(confStringElements, "--tmp-dir", "/cache")
	confStringElements = append(confStringElements, "--threads", "12")
	confStringElements = append(confStringElements, "--prefix", "result")
	confStringElements = append(confStringElements, "-o", "/output")

	if conf.HasProdigal {
		confStringElements = append(confStringElements, "--prodigal-tf", "/data/prodigaltraining.tf")
	}

	if conf.HasReplicons {
		if strings.HasSuffix(job.ProdigalKey, "csv") {
			confStringElements = append(confStringElements, "--replicons", "/data/replicons.csv")
		} else if strings.HasSuffix(job.ProdigalKey, "csv") {
			confStringElements = append(confStringElements, "--replicons", "/data/replicons.tsv")
		}
	}

	if viper.IsSet("Testing") || viper.IsSet("Debug") {
		confStringElements = append(confStringElements, "--db", "/db/db-mock")
	} else {
		confStringElements = append(confStringElements, "--db", "/db/db")
	}

	if conf.CompleteGenome {
		confStringElements = append(confStringElements, "--complete")
	}

	if conf.Locus != "" {
		confStringElements = append(confStringElements, "--locus", fmt.Sprintf("%v", conf.Locus))
	}

	if conf.LocusTag != "" {
		confStringElements = append(confStringElements, "--locus-tag", fmt.Sprintf("%v", conf.LocusTag))
	}

	if conf.KeepContigHeaders {
		confStringElements = append(confStringElements, "--keep-contig-headers")
	}

	if conf.Genus != "" {
		confStringElements = append(confStringElements, "--genus", fmt.Sprintf("%v", conf.Genus))
	}

	if conf.Species != "" {
		confStringElements = append(confStringElements, "--species", fmt.Sprintf("%v", conf.Species))
	}

	if conf.Strain != "" {
		confStringElements = append(confStringElements, "--strain", fmt.Sprintf("%v", conf.Strain))
	}

	if conf.Plasmid != "" {
		confStringElements = append(confStringElements, "--plasmid", fmt.Sprintf("%v", conf.Plasmid))
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

	//confString := strings.Join(confStringElements, " ")

	_, fastaFileName := path.Split(job.FastaKey)
	//confString = fmt.Sprintf(confString+" /data/%v", fastaFileName)

	confStringElements = append(confStringElements, fmt.Sprintf("/data/%v", fastaFileName))

	return json.Marshal(confStringElements)
}

//createUploadConf Creates the configuration string for a bakta job
func createUploadConf(job *database.Job) (string, error) {
	uploadStructType := reflect.TypeOf(objectStorage.UploadLinks{})

	var fields []string

	for i := 0; i < uploadStructType.NumField(); i++ {
		fieldFileSuffix := uploadStructType.Field(i).Tag.Get("bakta")
		fullFilename := strings.Join([]string{"/output/result", ".", fieldFileSuffix}, "")
		fields = append(fields, fullFilename)

	}

	allFiles := strings.Join(fields, ",")

	confString := fmt.Sprintf("upload -e s3.computational.bio.uni-giessen.de -k %v -b %v -f %v", job.ResultKey, job.DataBucket, allFiles)

	return confString, nil
}
