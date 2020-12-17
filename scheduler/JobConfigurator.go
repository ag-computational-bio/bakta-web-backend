package scheduler

import (
	"fmt"
	"path"

	"github.com/ag-computational-bio/bakta-web-api/go/api"

	"github.com/ag-computational-bio/bakta-web-backend/database"
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
func createBaktaConf(job *database.Job, conf *api.JobConfig) (string, error) {
	confString := "--db /db/db-mock --tmp-dir /cache --threads 8"

	_, fastaFileName := path.Split(job.FastaKey)
	confString = fmt.Sprintf(confString+" /data/%v", fastaFileName)

	return confString, nil
}

//createUploadConf Creates the configuration string for a bakta job
func createUploadConf(job *database.Job) (string, error) {
	confString := fmt.Sprintf("upload -e s3.computational.bio.uni-giessen.de -k %v -b %v -f /results.tar.gz", job.ResultKey, job.DataBucket)

	return confString, nil
}
