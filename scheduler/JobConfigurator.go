package scheduler

import (
	"fmt"
	"path"
	"reflect"
	"strings"

	"github.com/spf13/viper"

	"github.com/ag-computational-bio/bakta-web-api/go/api"

	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
)

//createDownloadConf Creates the configuration string for the download part of a bakta job
//The job has to be provided along with two bools that indicate if a prodigal training file and/or a replicon file are present
func createDownloadConf(job *database.Job, prodigaltf bool, replicontsv bool) (string, error) {
	keyString := job.FastaKey
	if prodigaltf {
		keyString = fmt.Sprintf(keyString+",%v", job.ProdigalKey)
	}
	if replicontsv {
		keyString = fmt.Sprintf(keyString+",%v", job.RepliconKey)
	}

	confString := fmt.Sprintf("download -b %v -k %v -d /data -e s3.computational.bio.uni-giessen.de", job.DataBucket, keyString)

	return confString, nil
}

//createBaktaConf Creates a bakta config string based on the configuration and job settings provided
func createBaktaConf(job *database.Job, conf *api.JobConfig, rawConfString string) (string, error) {
	var confStringElements []string

	if rawConfString != "" {
		confStringElements = append(confStringElements, rawConfString)
	}

	confStringElements = append(confStringElements, "--tmp-dir /cache")
	confStringElements = append(confStringElements, "--threads 8")
	confStringElements = append(confStringElements, "--prefix result")
	confStringElements = append(confStringElements, "-o /output")

	if viper.IsSet("Testing") || viper.IsSet("Debug") {
		confStringElements = append(confStringElements, "--db /db/db-mock")
	} else {
		confStringElements = append(confStringElements, "--db /db/db")
	}

	confString := strings.Join(confStringElements, " ")

	_, fastaFileName := path.Split(job.FastaKey)
	confString = fmt.Sprintf(confString+" /data/%v", fastaFileName)

	return confString, nil
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
